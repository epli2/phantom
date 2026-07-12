use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;

use phantom_core::query::TraceQuery;
use phantom_core::storage::TraceStore;
use phantom_core::trace::{SpanId, TraceId};
use phantom_core::view::{RenderOptions, TraceView};
use phantom_storage::FjallTraceStore;

use super::session::{CaptureManager, ChildState, SessionStatus};

const INSTRUCTIONS: &str = "phantom captures HTTP/HTTPS traffic from processes with zero \
instrumentation and stores every request/response pair as a queryable trace.\n\
Typical flow: call start_capture with the command to trace (e.g. [\"node\", \"app.js\"]); \
poll capture_status until its child has exited (or keep the session running for servers); \
then call list_traces with filters (method, status like \"5xx\", url_contains, since_ms) \
to inspect what the process did on the network. Use get_trace with a span_id for full \
request/response detail. Bodies are truncated by default to save context — raise max_body \
when you need more. Traces persist across sessions until clear_traces.";

// ─────────────────────────────────────────────────────────────────────────────
// Tool parameter types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartCaptureParams {
    /// Command to spawn and trace, as argv (e.g. ["node", "app.js"]).
    /// Node/PHP/Java get transparent HTTPS injection automatically.
    /// Empty or omitted = start a proxy-only session (configure clients
    /// with HTTP_PROXY=http://127.0.0.1:<port> yourself).
    #[serde(default)]
    pub command: Vec<String>,
    /// Proxy port; omit for an automatically chosen free port.
    pub port: Option<u16>,
    /// Disable TLS verification toward backend servers (self-signed certs).
    #[serde(default)]
    pub insecure: bool,
    /// Fault injection specs, e.g. "delay:100ms", "error:503:0.5:/api".
    #[serde(default)]
    pub fault: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StopCaptureParams {
    /// Session ID returned by start_capture.
    pub session_id: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct CaptureStatusParams {
    /// Session ID to inspect; omit for all sessions.
    pub session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ListTracesParams {
    /// Only these HTTP methods (e.g. ["GET", "POST"]); omit for all.
    #[serde(default)]
    pub method: Vec<String>,
    /// Status filter: exact ("404"), class ("4xx"), or range ("400-499").
    pub status: Option<String>,
    /// Only URLs containing this substring (case-insensitive).
    pub url_contains: Option<String>,
    /// Only traces at or after this Unix-epoch-milliseconds timestamp.
    pub since_ms: Option<u64>,
    /// Only traces at or before this Unix-epoch-milliseconds timestamp.
    pub until_ms: Option<u64>,
    /// Only spans of this 32-char hex trace ID.
    pub trace_id: Option<String>,
    /// Maximum traces to return (default 20).
    pub limit: Option<u32>,
    /// Matching traces to skip, for pagination (default 0).
    pub offset: Option<u32>,
    /// Truncate bodies to this many bytes (default 256; 0 = omit nothing).
    /// Truncated bodies carry *_body_truncated: true and *_body_bytes.
    pub max_body: Option<u32>,
    /// Omit bodies entirely (sizes still reported).
    #[serde(default)]
    pub headers_only: bool,
    /// Replace authorization/cookie-style header values with "[redacted]"
    /// (default true).
    pub redact_sensitive_headers: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetTraceParams {
    /// 16-char hex span ID (from list_traces).
    pub span_id: String,
    /// Truncate bodies to this many bytes (default 4096; 0 = unlimited).
    pub max_body: Option<u32>,
    /// Omit bodies entirely (sizes still reported).
    #[serde(default)]
    pub headers_only: bool,
    /// Replace authorization/cookie-style header values with "[redacted]"
    /// (default true).
    pub redact_sensitive_headers: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClearTracesParams {
    /// Must be true; guards against accidental deletion.
    pub confirm: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Server
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PhantomMcp {
    store: Arc<FjallTraceStore>,
    sessions: Arc<CaptureManager>,
    data_dir: PathBuf,
    tool_router: ToolRouter<Self>,
}

fn json_result(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::json(value)?]))
}

fn internal_error(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn invalid_params(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

fn session_status_json(s: &SessionStatus) -> serde_json::Value {
    let (state, exit_code) = match s.child_state {
        ChildState::None => ("proxy_only", None),
        ChildState::Running => ("running", None),
        ChildState::Exited(code) => ("exited", code),
    };
    serde_json::json!({
        "session_id": s.id,
        "port": s.port,
        "proxy_url": format!("http://127.0.0.1:{}", s.port),
        "command": s.command,
        "pid": s.child_pid,
        "state": state,
        "exit_code": exit_code,
        "trace_count": s.trace_count,
        "started_at_ms": s.started_at_ms,
    })
}

fn render_options(
    max_body: Option<u32>,
    default_max_body: usize,
    headers_only: bool,
    redact_sensitive: Option<bool>,
) -> RenderOptions {
    let max_body = match max_body {
        Some(0) => None,
        Some(n) => Some(n as usize),
        None => Some(default_max_body),
    };
    RenderOptions {
        max_body,
        headers_only,
        redact_headers: if redact_sensitive.unwrap_or(true) {
            RenderOptions::sensitive_headers()
        } else {
            Vec::new()
        },
    }
}

#[tool_router(router = tool_router)]
impl PhantomMcp {
    pub fn new(
        store: Arc<FjallTraceStore>,
        sessions: Arc<CaptureManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            store,
            sessions,
            data_dir,
            tool_router: Self::tool_router(),
        }
    }

    /// Runs a store query on a blocking thread (fjall is synchronous).
    async fn query_store<T, F>(&self, f: F) -> Result<T, McpError>
    where
        T: Send + 'static,
        F: FnOnce(&FjallTraceStore) -> Result<T, phantom_core::error::StorageError>
            + Send
            + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(internal_error)?
            .map_err(internal_error)
    }

    #[tool(
        description = "Start capturing HTTP(S) traffic. Spawns the given command routed through a MITM proxy (Node/PHP/Java get transparent HTTPS injection), or starts a proxy-only session when no command is given. Returns the session_id and proxy port. The session keeps capturing after the child exits, until stop_capture."
    )]
    async fn start_capture(
        &self,
        Parameters(p): Parameters<StartCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        let status = self
            .sessions
            .start(self.store.clone(), p.command, p.port, p.insecure, &p.fault)
            .await
            .map_err(internal_error)?;
        json_result(session_status_json(&status))
    }

    #[tool(
        description = "Stop a capture session: kills a still-running traced child and shuts the proxy down. Returns the final session status including the child's exit_code."
    )]
    async fn stop_capture(
        &self,
        Parameters(p): Parameters<StopCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.sessions.stop(&p.session_id) {
            Some(status) => json_result(session_status_json(&status)),
            None => Err(invalid_params(format!(
                "unknown session ID {:?}",
                p.session_id
            ))),
        }
    }

    #[tool(
        description = "Status of capture sessions: child state (running/exited + exit_code), trace_count so far, port. Pass session_id for one session, omit for all. Poll this after start_capture to detect when the traced command finished."
    )]
    async fn capture_status(
        &self,
        Parameters(p): Parameters<CaptureStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let statuses = self.sessions.status(p.session_id.as_deref());
        if p.session_id.is_some() && statuses.is_empty() {
            return Err(invalid_params("unknown session ID"));
        }
        json_result(serde_json::json!({
            "sessions": statuses.iter().map(session_status_json).collect::<Vec<_>>(),
        }))
    }

    #[tool(
        description = "List captured traces, newest first, with filters. Bodies are truncated to max_body bytes (default 256) to protect context — use get_trace for full detail. Returns request/response summary per trace including span_id."
    )]
    async fn list_traces(
        &self,
        Parameters(p): Parameters<ListTracesParams>,
    ) -> Result<CallToolResult, McpError> {
        let methods = p
            .method
            .iter()
            .map(|m| m.parse())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e: phantom_core::trace::ParseMethodError| invalid_params(e.to_string()))?;
        let status = p.status.as_deref().map(|s| s.parse()).transpose().map_err(
            |e: phantom_core::query::ParseStatusRangeError| invalid_params(e.to_string()),
        )?;
        let trace_id = p
            .trace_id
            .as_deref()
            .map(|s| {
                TraceId::from_hex(s)
                    .ok_or_else(|| invalid_params("invalid trace_id: expected 32 hex chars"))
            })
            .transpose()?;
        let to_time = |ms: u64| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms);

        let query = TraceQuery {
            methods,
            status,
            url_contains: p.url_contains,
            since: p.since_ms.map(to_time),
            until: p.until_ms.map(to_time),
            trace_id,
            limit: p.limit.unwrap_or(20) as usize,
            offset: p.offset.unwrap_or(0) as usize,
        };
        let traces = self.query_store(move |s| s.query(&query)).await?;

        let opts = render_options(p.max_body, 256, p.headers_only, p.redact_sensitive_headers);
        let views: Vec<TraceView> = traces.iter().map(|t| TraceView::render(t, &opts)).collect();
        json_result(serde_json::json!({ "traces": views }))
    }

    #[tool(
        description = "Fetch one trace by its 16-char hex span_id with full request/response detail. Bodies are truncated to max_body bytes (default 4096); pass max_body: 0 for the complete body."
    )]
    async fn get_trace(
        &self,
        Parameters(p): Parameters<GetTraceParams>,
    ) -> Result<CallToolResult, McpError> {
        let span_id = SpanId::from_hex(&p.span_id)
            .ok_or_else(|| invalid_params("invalid span_id: expected 16 hex chars"))?;
        let trace = self
            .query_store(move |s| s.get_by_span_id(&span_id))
            .await?
            .ok_or_else(|| invalid_params(format!("no trace found for span ID {}", p.span_id)))?;

        let opts = render_options(p.max_body, 4096, p.headers_only, p.redact_sensitive_headers);
        json_result(serde_json::to_value(TraceView::render(&trace, &opts)).map_err(internal_error)?)
    }

    #[tool(
        description = "Trace store statistics: total stored traces (approximate), data directory, number of active capture sessions."
    )]
    async fn get_stats(&self) -> Result<CallToolResult, McpError> {
        let total = self.query_store(|s| s.count()).await?;
        json_result(serde_json::json!({
            "total_traces": total,
            "data_dir": self.data_dir.display().to_string(),
            "active_sessions": self.sessions.active_count(),
        }))
    }

    #[tool(
        description = "Delete all stored traces. Requires confirm: true. Traces captured by still-running sessions after the clear are kept."
    )]
    async fn clear_traces(
        &self,
        Parameters(p): Parameters<ClearTracesParams>,
    ) -> Result<CallToolResult, McpError> {
        if !p.confirm {
            return Err(invalid_params("pass confirm: true to delete all traces"));
        }
        self.query_store(|s| s.clear()).await?;
        json_result(serde_json::json!({ "cleared": true }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PhantomMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serves MCP over stdio until the client disconnects, then tears down all
/// capture sessions.
pub async fn run_mcp(store: Arc<FjallTraceStore>, data_dir: PathBuf) -> anyhow::Result<()> {
    let sessions = Arc::new(CaptureManager::default());
    let server = PhantomMcp::new(store, sessions.clone(), data_dir);
    let service = server
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("MCP server failed to start: {e}"))?;
    let quit_reason = service.waiting().await?;
    tracing::info!("MCP server stopped: {quit_reason:?}");
    sessions.stop_all();
    Ok(())
}
