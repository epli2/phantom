//! LD_PRELOAD capture backend — Linux only.
//!
//! Listens on a Unix datagram socket for JSON messages emitted by the
//! phantom-agent dylib injected into a target process, and converts them into
//! [`HttpTrace`] or [`MysqlTrace`] objects.
//!
//! Messages are discriminated by the `msg_type` field:
//! - `"mysql"` → [`MysqlTrace`] emitted on the MySQL channel
//! - anything else (or absent) → [`HttpTrace`] emitted on the HTTP channel

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use phantom_core::capture::CaptureBackend;
use phantom_core::error::CaptureError;
use phantom_core::mysql::{MysqlResponseKind, MysqlStore, MysqlTrace};
use phantom_core::trace::{HttpMethod, HttpTrace, SpanId, TraceId};
use tokio::net::UnixDatagram;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

// ─────────────────────────────────────────────────────────────────────────────
// IPC message format — HTTP (must match phantom-agent's TraceMsg)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AgentTrace {
    method: String,
    url: String,
    status_code: u16,
    request_headers: HashMap<String, String>,
    response_headers: HashMap<String, String>,
    request_body_b64: Option<String>,
    response_body_b64: Option<String>,
    duration_ms: u64,
    timestamp_ms: u64,
    dest_addr: Option<String>,
    #[serde(default)]
    protocol_version: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// IPC message format — MySQL (must match phantom-agent's MysqlTraceMsg)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AgentMysqlTrace {
    query: String,
    duration_ms: u64,
    timestamp_ms: u64,
    dest_addr: Option<String>,
    db_name: Option<String>,
    // OK fields
    affected_rows: Option<u64>,
    last_insert_id: Option<u64>,
    warnings: Option<u16>,
    // ResultSet fields
    column_count: Option<u64>,
    row_count: Option<u64>,
    // ERR fields
    error_code: Option<u16>,
    sql_state: Option<String>,
    error_message: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversion helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_method(s: &str) -> HttpMethod {
    match s.to_uppercase().as_str() {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        "PATCH" => HttpMethod::Patch,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        "TRACE" => HttpMethod::Trace,
        "CONNECT" => HttpMethod::Connect,
        _ => HttpMethod::Get,
    }
}

fn decode_body(b64: Option<String>) -> Option<Vec<u8>> {
    b64.and_then(|s| B64.decode(s).ok())
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    buf.iter_mut().for_each(|b| *b = rand::random());
    buf
}

fn ms_to_system_time(ms: u64) -> SystemTime {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
    if ts < UNIX_EPOCH {
        SystemTime::now()
    } else {
        ts
    }
}

fn agent_trace_to_http_trace(a: AgentTrace) -> HttpTrace {
    HttpTrace {
        span_id: SpanId(rand_bytes::<8>()),
        trace_id: TraceId(rand_bytes::<16>()),
        parent_span_id: None,
        method: parse_method(&a.method),
        url: a.url,
        request_headers: a.request_headers,
        request_body: decode_body(a.request_body_b64),
        status_code: a.status_code,
        response_headers: a.response_headers,
        response_body: decode_body(a.response_body_b64),
        timestamp: ms_to_system_time(a.timestamp_ms),
        duration: Duration::from_millis(a.duration_ms),
        source_addr: None,
        dest_addr: a.dest_addr,
        protocol_version: a.protocol_version.unwrap_or_else(|| "HTTP/1.1".to_string()),
    }
}

fn agent_mysql_trace_to_mysql_trace(a: AgentMysqlTrace) -> MysqlTrace {
    let response = if let Some(code) = a.error_code {
        MysqlResponseKind::Err {
            error_code: code,
            sql_state: a.sql_state.unwrap_or_default(),
            message: a.error_message.unwrap_or_default(),
        }
    } else if a.column_count.is_some() {
        MysqlResponseKind::ResultSet {
            column_count: a.column_count.unwrap_or(0),
            row_count: a.row_count.unwrap_or(0),
        }
    } else {
        MysqlResponseKind::Ok {
            affected_rows: a.affected_rows.unwrap_or(0),
            last_insert_id: a.last_insert_id.unwrap_or(0),
            warnings: a.warnings.unwrap_or(0),
        }
    };

    MysqlTrace {
        span_id: SpanId(rand_bytes::<8>()),
        trace_id: TraceId(rand_bytes::<16>()),
        parent_span_id: None,
        query: a.query,
        response,
        timestamp: ms_to_system_time(a.timestamp_ms),
        duration: Duration::from_millis(a.duration_ms),
        dest_addr: a.dest_addr,
        db_name: a.db_name,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LdPreloadCaptureBackend
// ─────────────────────────────────────────────────────────────────────────────

pub struct LdPreloadCaptureBackend {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl LdPreloadCaptureBackend {
    /// Create a new backend that will bind to `socket_path`.
    ///
    /// Call [`socket_path()`][Self::socket_path] before [`start()`][CaptureBackend::start]
    /// to obtain the path to pass in `PHANTOM_SOCKET` when spawning the target process.
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            shutdown_tx: None,
            task_handle: None,
        }
    }

    /// The Unix socket path agents must write to (`PHANTOM_SOCKET` env var).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Start capturing, returning **both** an HTTP and a MySQL trace receiver.
    ///
    /// This is the preferred entry point when running the LD_PRELOAD backend.
    /// The [`CaptureBackend::start`] implementation calls this and discards the
    /// MySQL receiver, preserving the trait contract for contexts that only need HTTP.
    pub fn start_mysql_aware(
        &mut self,
    ) -> Result<(mpsc::Receiver<HttpTrace>, mpsc::Receiver<MysqlTrace>), CaptureError> {
        // Remove stale socket file if it exists.
        let _ = std::fs::remove_file(&self.socket_path);

        let socket = UnixDatagram::bind(&self.socket_path)
            .map_err(|e| CaptureError::StartFailed(e.to_string()))?;

        let (http_tx, http_rx) = mpsc::channel::<HttpTrace>(4096);
        let (mysql_tx, mysql_rx) = mpsc::channel::<MysqlTrace>(4096);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let task_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((n, _from)) => {
                                dispatch_agent_message(
                                    &buf[..n],
                                    &http_tx,
                                    &mysql_tx,
                                );
                            }
                            Err(e) => {
                                warn!("ldpreload socket recv error: {e}");
                                break;
                            }
                        }
                    }
                }
            }
        });

        self.shutdown_tx = Some(shutdown_tx);
        self.task_handle = Some(task_handle);
        Ok((http_rx, mysql_rx))
    }
}

/// Peek at the `msg_type` field and dispatch to the appropriate channel.
fn dispatch_agent_message(
    data: &[u8],
    http_tx: &mpsc::Sender<HttpTrace>,
    mysql_tx: &mpsc::Sender<MysqlTrace>,
) {
    // Deserialise into a generic Value to inspect msg_type without duplicating
    // the full struct. For MySQL messages the overhead is negligible.
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(data) else {
        warn!("ldpreload: failed to parse agent message as JSON");
        return;
    };

    match val.get("msg_type").and_then(|v| v.as_str()) {
        Some("mysql") => match serde_json::from_value::<AgentMysqlTrace>(val) {
            Ok(agent) => {
                let trace = agent_mysql_trace_to_mysql_trace(agent);
                debug!(query = %trace.query, "mysql trace captured via ldpreload");
                if mysql_tx.try_send(trace).is_err() {
                    warn!("ldpreload mysql trace channel full, dropping");
                }
            }
            Err(e) => warn!("ldpreload: failed to parse mysql message: {e}"),
        },
        _ => {
            // No msg_type or msg_type != "mysql" → treat as HTTP trace.
            match serde_json::from_value::<AgentTrace>(val) {
                Ok(agent) => {
                    let trace = agent_trace_to_http_trace(agent);
                    debug!(url = %trace.url, "captured via ldpreload");
                    if http_tx.try_send(trace).is_err() {
                        warn!("ldpreload trace channel full, dropping");
                    }
                }
                Err(e) => warn!("ldpreload: failed to parse http message: {e}"),
            }
        }
    }
}

impl CaptureBackend for LdPreloadCaptureBackend {
    fn start(&mut self) -> Result<mpsc::Receiver<HttpTrace>, CaptureError> {
        let (http_rx, _mysql_rx) = self.start_mysql_aware()?;
        Ok(http_rx)
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }

    fn name(&self) -> &str {
        "ldpreload"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — MysqlStore trait is used here to keep the import active
// ─────────────────────────────────────────────────────────────────────────────

// Suppress unused import warning: MysqlStore is re-exported for use by main.rs
const _: fn() = || {
    let _: Option<&dyn MysqlStore> = None;
};
