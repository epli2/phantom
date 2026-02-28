//! LD_PRELOAD capture backend — Linux only.
//!
//! Listens on a Unix datagram socket for [`TraceMsg`] JSON messages emitted
//! by the phantom-agent dylib injected into a target process, and converts
//! them into [`HttpTrace`] objects.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use phantom_core::capture::CaptureBackend;
use phantom_core::error::CaptureError;
use phantom_core::trace::{HttpMethod, HttpTrace, SpanId, TraceId};
use tokio::net::UnixDatagram;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

// ─────────────────────────────────────────────────────────────────────────────
// IPC message format (must match phantom-agent's TraceMsg)
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
}

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

fn agent_trace_to_http_trace(a: AgentTrace) -> HttpTrace {
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_millis(a.timestamp_ms);
    // Guard against timestamps before UNIX_EPOCH (shouldn't happen but be safe).
    let timestamp = if timestamp < UNIX_EPOCH {
        SystemTime::now()
    } else {
        timestamp
    };

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
        timestamp,
        duration: Duration::from_millis(a.duration_ms),
        source_addr: None,
        dest_addr: a.dest_addr,
        protocol_version: "HTTP/1.1".to_string(),
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
}

impl CaptureBackend for LdPreloadCaptureBackend {
    fn start(&mut self) -> Result<mpsc::Receiver<HttpTrace>, CaptureError> {
        // Remove stale socket file if it exists.
        let _ = std::fs::remove_file(&self.socket_path);

        let socket = UnixDatagram::bind(&self.socket_path)
            .map_err(|e| CaptureError::StartFailed(e.to_string()))?;

        let (trace_tx, trace_rx) = mpsc::channel(4096);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let task_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((n, _from)) => {
                                match serde_json::from_slice::<AgentTrace>(&buf[..n]) {
                                    Ok(agent_trace) => {
                                        let trace = agent_trace_to_http_trace(agent_trace);
                                        debug!(url = %trace.url, "captured via ldpreload");
                                        if trace_tx.try_send(trace).is_err() {
                                            warn!("ldpreload trace channel full, dropping");
                                        }
                                    }
                                    Err(e) => {
                                        warn!("ldpreload: failed to parse agent message: {e}");
                                    }
                                }
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
        Ok(trace_rx)
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
