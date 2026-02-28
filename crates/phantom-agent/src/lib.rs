//! Phantom LD_PRELOAD agent — Linux only.
//!
//! Build as a `dylib` and inject with:
//!   `LD_PRELOAD=/path/to/libphantom_agent.so PHANTOM_SOCKET=/tmp/phantom.sock <cmd>`
//!
//! The agent hooks `send()` / `recv()` / `close()` from libc to intercept
//! plain-text HTTP/1.x traffic. Captured traces are sent as JSON datagrams
//! over a Unix datagram socket to the phantom main process.
//!
//! **Limitation**: HTTPS traffic is encrypted at the socket layer and cannot
//! be captured this way (would require hooking OpenSSL/GnuTLS functions).

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::collections::HashMap;
use std::os::unix::net::UnixDatagram;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use libc::{c_int, c_void, size_t, ssize_t};

// ─────────────────────────────────────────────────────────────────────────────
// Re-entry guard — prevents recursive hook calls (e.g. if our code calls send)
// ─────────────────────────────────────────────────────────────────────────────

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

// ─────────────────────────────────────────────────────────────────────────────
// IPC — send JSON datagrams to phantom via UnixDatagram::send_to()
//
// We intentionally use `send_to()` (→ `sendto()` syscall) rather than
// `send()` to avoid re-entering our own `send` hook.
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum datagram payload.  Linux UDS datagrams are limited to ~64 KB.
const MAX_DATAGRAM: usize = 60_000;
/// Maximum body bytes stored per trace (keeps datagrams small).
const MAX_BODY: usize = 16_384;
/// Maximum bytes we buffer per FD before giving up.
const MAX_BUF: usize = 512 * 1024;

static IPC_SOCKET: OnceLock<Option<(UnixDatagram, String)>> = OnceLock::new();

fn ipc() -> Option<(&'static UnixDatagram, &'static str)> {
    IPC_SOCKET
        .get_or_init(|| {
            let path = std::env::var("PHANTOM_SOCKET").ok()?;
            // `unbound()` creates an anonymous datagram socket.
            // `send_to()` on it calls sendto(2), NOT send(2) — safe from recursion.
            let sock = UnixDatagram::unbound().ok()?;
            Some((sock, path))
        })
        .as_ref()
        .map(|(s, p)| (s, p.as_str()))
}

// ─────────────────────────────────────────────────────────────────────────────
// IPC message (serialised to JSON and sent as a datagram)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct TraceMsg {
    method: String,
    url: String,
    status_code: u16,
    request_headers: HashMap<String, String>,
    response_headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_body_b64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body_b64: Option<String>,
    duration_ms: u64,
    timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    dest_addr: Option<String>,
}

fn emit_msg(msg: &TraceMsg) {
    let Some((sock, path)) = ipc() else { return };
    let Ok(data) = serde_json::to_vec(msg) else {
        return;
    };
    if data.len() <= MAX_DATAGRAM {
        let _ = sock.send_to(&data, path);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Base64 encoder (avoids adding an external crate to the dylib)
// ─────────────────────────────────────────────────────────────────────────────

fn b64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn body_b64(raw: &[u8]) -> Option<String> {
    if raw.is_empty() {
        None
    } else {
        let trunc = &raw[..raw.len().min(MAX_BODY)];
        Some(b64_encode(trunc))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-FD state machine
// ─────────────────────────────────────────────────────────────────────────────

struct ReqInfo {
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    started_at: Instant,
    timestamp_ms: u64,
}

enum FdState {
    /// Still accumulating the HTTP request bytes.
    CollectingRequest { buf: Vec<u8> },
    /// Request fully parsed; accumulating HTTP response bytes.
    CollectingResponse {
        req: ReqInfo,
        buf: Vec<u8>,
        // Populated once response headers are parsed:
        status_code: Option<u16>,
        resp_headers: Option<HashMap<String, String>>,
        content_length: Option<usize>,
        headers_end: Option<usize>,
    },
}

static FD_MAP: OnceLock<Mutex<HashMap<i32, FdState>>> = OnceLock::new();

fn fd_map() -> &'static Mutex<HashMap<i32, FdState>> {
    FD_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP detection
// ─────────────────────────────────────────────────────────────────────────────

const HTTP_METHODS: &[&[u8]] = &[
    b"GET ",
    b"POST ",
    b"PUT ",
    b"DELETE ",
    b"PATCH ",
    b"HEAD ",
    b"OPTIONS ",
    b"TRACE ",
    b"CONNECT ",
];

fn looks_like_http_request(data: &[u8]) -> bool {
    HTTP_METHODS.iter().any(|m| data.starts_with(m))
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP parsing helpers (using httparse)
// ─────────────────────────────────────────────────────────────────────────────

fn try_parse_request(buf: &[u8]) -> Option<ReqInfo> {
    let mut headers_storage = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers_storage);
    let httparse::Status::Complete(headers_end) = req.parse(buf).ok()? else {
        return None;
    };

    let method = req.method?.to_string();
    let path = req.path?.to_string();
    let mut hmap = HashMap::new();
    let mut host = String::new();
    let mut content_length = 0usize;

    for h in req.headers.iter() {
        let name = h.name.to_lowercase();
        let value = String::from_utf8_lossy(h.value).into_owned();
        if name == "host" {
            host = value.clone();
        }
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        hmap.insert(name, value);
    }

    let url = if path.starts_with("http://") || path.starts_with("https://") {
        path
    } else {
        format!("http://{host}{path}")
    };

    // Only take body bytes that are already in the buffer.
    let body_end = (headers_end + content_length).min(buf.len());
    let body = buf[headers_end..body_end].to_vec();

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Some(ReqInfo {
        method,
        url,
        headers: hmap,
        body,
        started_at: Instant::now(),
        timestamp_ms: ts,
    })
}

struct RespMeta {
    status_code: u16,
    headers: HashMap<String, String>,
    content_length: Option<usize>,
    headers_end: usize,
}

fn try_parse_response_headers(buf: &[u8]) -> Option<RespMeta> {
    let mut headers_storage = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers_storage);
    let httparse::Status::Complete(headers_end) = resp.parse(buf).ok()? else {
        return None;
    };

    let status_code = resp.code?;
    let mut hmap = HashMap::new();
    let mut content_length: Option<usize> = None;
    let mut is_chunked = false;

    for h in resp.headers.iter() {
        let name = h.name.to_lowercase();
        let value = String::from_utf8_lossy(h.value).into_owned();
        if name == "content-length" {
            content_length = value.parse().ok();
        }
        if name == "transfer-encoding" && value.to_lowercase().contains("chunked") {
            is_chunked = true;
        }
        hmap.insert(name, value);
    }

    // For chunked responses we wait until the connection closes.
    if is_chunked {
        content_length = None;
    }

    Some(RespMeta {
        status_code,
        headers: hmap,
        content_length,
        headers_end,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Emit a completed trace
// ─────────────────────────────────────────────────────────────────────────────

fn do_emit(
    req: ReqInfo,
    status_code: u16,
    resp_headers: HashMap<String, String>,
    resp_body: &[u8],
    duration: Duration,
) {
    emit_msg(&TraceMsg {
        method: req.method,
        url: req.url,
        status_code,
        request_headers: req.headers,
        response_headers: resp_headers,
        request_body_b64: body_b64(&req.body),
        response_body_b64: body_b64(resp_body),
        duration_ms: duration.as_millis() as u64,
        timestamp_ms: req.timestamp_ms,
        dest_addr: None,
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Hook processing (called from within hooks, after re-entry check)
// ─────────────────────────────────────────────────────────────────────────────

fn process_send(fd: i32, data: &[u8]) {
    let mut map = match fd_map().lock() {
        Ok(m) => m,
        Err(_) => return,
    };

    if looks_like_http_request(data) {
        // Start fresh tracking for this fd (may overwrite stale state).
        let buf = data.to_vec();
        if let Some(req_info) = try_parse_request(&buf) {
            map.insert(
                fd,
                FdState::CollectingResponse {
                    req: req_info,
                    buf: Vec::new(),
                    status_code: None,
                    resp_headers: None,
                    content_length: None,
                    headers_end: None,
                },
            );
        } else {
            map.insert(fd, FdState::CollectingRequest { buf });
        }
    } else {
        // Possible continuation of an incomplete request.
        // Use a flag to avoid holding the borrow when we call map.insert().
        let transition = if let Some(FdState::CollectingRequest { buf }) = map.get_mut(&fd) {
            if buf.len() < MAX_BUF {
                buf.extend_from_slice(data);
            }
            try_parse_request(buf) // returns owned ReqInfo if complete
        } else {
            return; // not tracking this fd
        };

        // Borrow of map.get_mut() ends here (transition is owned).
        if let Some(req_info) = transition {
            map.insert(
                fd,
                FdState::CollectingResponse {
                    req: req_info,
                    buf: Vec::new(),
                    status_code: None,
                    resp_headers: None,
                    content_length: None,
                    headers_end: None,
                },
            );
        }
    }
}

fn process_recv(fd: i32, data: &[u8]) {
    // Phase 1: accumulate, parse headers if ready, check completeness.
    // Return owned FdState if the response is complete (to emit outside the lock).
    let to_emit = {
        let mut map = match fd_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };

        let complete = match map.get_mut(&fd) {
            Some(FdState::CollectingResponse {
                buf,
                status_code,
                resp_headers,
                content_length,
                headers_end,
                ..
            }) => {
                if buf.len() < MAX_BUF {
                    buf.extend_from_slice(data);
                }

                // Parse response headers once.
                if headers_end.is_none() {
                    if let Some(meta) = try_parse_response_headers(buf) {
                        *status_code = Some(meta.status_code);
                        *resp_headers = Some(meta.headers);
                        *content_length = meta.content_length;
                        *headers_end = Some(meta.headers_end);
                    }
                }

                // Check if we have Content-Length bytes of body.
                matches!(
                    (*content_length, *headers_end),
                    (Some(cl), Some(he)) if buf.len() >= he + cl
                )
            }
            _ => false,
        };

        if complete {
            map.remove(&fd) // → Option<FdState>, owned
        } else {
            None
        }
    }; // Lock released here

    if let Some(FdState::CollectingResponse {
        req,
        buf,
        status_code: Some(sc),
        resp_headers: Some(rh),
        content_length,
        headers_end: Some(he),
    }) = to_emit
    {
        let cl = content_length.unwrap_or(0);
        let body_end = (he + cl).min(buf.len());
        let duration = req.started_at.elapsed();
        do_emit(req, sc, rh, &buf[he..body_end], duration);
    }
}

fn process_close(fd: i32) {
    let state = {
        let mut map = match fd_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        map.remove(&fd)
    }; // Lock released

    // Emit partial response (e.g. chunked or connection-close semantics).
    if let Some(FdState::CollectingResponse {
        req,
        buf,
        status_code: Some(sc),
        resp_headers: Some(rh),
        content_length,
        headers_end: Some(he),
    }) = state
    {
        let cl = content_length.unwrap_or_else(|| buf.len().saturating_sub(he));
        let body_end = (he + cl).min(buf.len());
        let duration = req.started_at.elapsed();
        do_emit(req, sc, rh, &buf[he..body_end], duration);
    }
    // If headers were never parsed, we have nothing useful to emit.
}

// ─────────────────────────────────────────────────────────────────────────────
// Hooks
// ─────────────────────────────────────────────────────────────────────────────

redhook::hook! {
    unsafe fn send(
        sockfd: c_int,
        buf:    *const c_void,
        len:    size_t,
        flags:  c_int
    ) -> ssize_t => phantom_send {
        let result = redhook::real!(send)(sockfd, buf, len, flags);
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    let data = std::slice::from_raw_parts(buf as *const u8, result as usize);
                    process_send(sockfd, data);
                    g.set(false);
                }
            });
        }
        result
    }
}

redhook::hook! {
    unsafe fn recv(
        sockfd: c_int,
        buf:    *mut c_void,
        len:    size_t,
        flags:  c_int
    ) -> ssize_t => phantom_recv {
        let result = redhook::real!(recv)(sockfd, buf, len, flags);
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    let data = std::slice::from_raw_parts(buf as *const u8, result as usize);
                    process_recv(sockfd, data);
                    g.set(false);
                }
            });
        }
        result
    }
}

redhook::hook! {
    unsafe fn close(fd: c_int) -> c_int => phantom_close {
        let result = redhook::real!(close)(fd);
        IN_HOOK.with(|g| {
            if !g.get() {
                g.set(true);
                process_close(fd);
                g.set(false);
            }
        });
        result
    }
}
