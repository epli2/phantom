//! Phantom LD_PRELOAD agent — Linux only.
//!
//! Build as a `dylib` and inject with:
//!   `LD_PRELOAD=/path/to/libphantom_agent.so PHANTOM_SOCKET=/tmp/phantom.sock <cmd>`
//!
//! The agent hooks `send()` / `recv()` / `close()` from libc to intercept
//! plain-text HTTP/1.x traffic, and `SSL_write()` / `SSL_read()` / `SSL_free()`
//! from OpenSSL/LibreSSL/BoringSSL to intercept HTTPS traffic (plaintext above
//! the TLS layer). Captured traces are sent as JSON datagrams over a Unix
//! datagram socket to the phantom main process.
//!
//! **Note**: HTTPS capture requires the target to dynamically link `libssl`.
//! Statically-linked TLS (e.g. Go's native crypto, Rust's rustls) is not
//! captured — use the proxy backend for those cases.

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::collections::HashMap;
use std::os::unix::net::UnixDatagram;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use libc::{c_int, c_void, size_t, ssize_t};

// ─────────────────────────────────────────────────────────────────────────────
// Re-entry guard — prevents recursive hook calls (e.g. if our code calls send,
// or SSL_write internally calls send)
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
/// Maximum bytes we buffer per connection before giving up.
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
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
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
// Per-connection state machine
//
// Keyed by `usize`: file descriptors (small integers) for plain sockets,
// SSL* pointer addresses (large heap values) for TLS connections.
// No collision is possible because FDs are in [0, 65535] and heap pointers
// are well above that range on 64-bit systems.
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
    CollectingRequest { buf: Vec<u8>, tls: bool },
    /// Request fully parsed; accumulating HTTP response bytes.
    CollectingResponse {
        req: Box<ReqInfo>,
        buf: Vec<u8>,
        tls: bool,
        // Populated once response headers are parsed:
        status_code: Option<u16>,
        resp_headers: Option<HashMap<String, String>>,
        content_length: Option<usize>,
        headers_end: Option<usize>,
    },
}

static STATE_MAP: OnceLock<Mutex<HashMap<usize, FdState>>> = OnceLock::new();

fn state_map() -> &'static Mutex<HashMap<usize, FdState>> {
    STATE_MAP.get_or_init(|| Mutex::new(HashMap::new()))
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
    tls: bool,
) {
    let url = if tls && req.url.starts_with("http://") {
        req.url.replacen("http://", "https://", 1)
    } else {
        req.url
    };
    emit_msg(&TraceMsg {
        method: req.method,
        url,
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
//
// `key` is either an FD (cast to usize) or an SSL* pointer (cast to usize).
// `tls` indicates whether the data comes from an SSL function.
// ─────────────────────────────────────────────────────────────────────────────

fn process_outgoing(key: usize, data: &[u8], tls: bool) {
    let mut map = match state_map().lock() {
        Ok(m) => m,
        Err(_) => return,
    };

    if looks_like_http_request(data) {
        // Start fresh tracking for this key (may overwrite stale state).
        let buf = data.to_vec();
        if let Some(req_info) = try_parse_request(&buf) {
            map.insert(
                key,
                FdState::CollectingResponse {
                    req: Box::new(req_info),
                    buf: Vec::new(),
                    tls,
                    status_code: None,
                    resp_headers: None,
                    content_length: None,
                    headers_end: None,
                },
            );
        } else {
            map.insert(key, FdState::CollectingRequest { buf, tls });
        }
    } else {
        // Possible continuation of an incomplete request.
        let transition = if let Some(FdState::CollectingRequest { buf, .. }) = map.get_mut(&key) {
            if buf.len() < MAX_BUF {
                buf.extend_from_slice(data);
            }
            try_parse_request(buf) // returns owned ReqInfo if complete
        } else {
            return; // not tracking this key
        };

        // Borrow of map.get_mut() ends here (transition is owned).
        if let Some(req_info) = transition {
            map.insert(
                key,
                FdState::CollectingResponse {
                    req: Box::new(req_info),
                    buf: Vec::new(),
                    tls,
                    status_code: None,
                    resp_headers: None,
                    content_length: None,
                    headers_end: None,
                },
            );
        }
    }
}

fn process_incoming(key: usize, data: &[u8]) {
    // Phase 1: accumulate, parse headers if ready, check completeness.
    // Return owned FdState if the response is complete (to emit outside the lock).
    let to_emit = {
        let mut map = match state_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };

        let complete = match map.get_mut(&key) {
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
                #[allow(clippy::collapsible_if)]
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
            map.remove(&key) // → Option<FdState>, owned
        } else {
            None
        }
    }; // Lock released here

    if let Some(FdState::CollectingResponse {
        req,
        buf,
        tls,
        status_code: Some(sc),
        resp_headers: Some(rh),
        content_length,
        headers_end: Some(he),
    }) = to_emit
    {
        let cl = content_length.unwrap_or(0);
        let body_end = (he + cl).min(buf.len());
        let duration = req.started_at.elapsed();
        do_emit(*req, sc, rh, &buf[he..body_end], duration, tls);
    }
}

fn process_teardown(key: usize) {
    let state = {
        let mut map = match state_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        map.remove(&key)
    }; // Lock released

    // Emit partial response (e.g. chunked or connection-close semantics).
    if let Some(FdState::CollectingResponse {
        req,
        buf,
        tls,
        status_code: Some(sc),
        resp_headers: Some(rh),
        content_length,
        headers_end: Some(he),
    }) = state
    {
        let cl = content_length.unwrap_or_else(|| buf.len().saturating_sub(he));
        let body_end = (he + cl).min(buf.len());
        let duration = req.started_at.elapsed();
        do_emit(*req, sc, rh, &buf[he..body_end], duration, tls);
    }
    // If headers were never parsed, we have nothing useful to emit.
}

// ─────────────────────────────────────────────────────────────────────────────
// Hooks — libc (plain HTTP)
// ─────────────────────────────────────────────────────────────────────────────

redhook::hook! {
    unsafe fn send(
        sockfd: c_int,
        buf:    *const c_void,
        len:    size_t,
        flags:  c_int
    ) -> ssize_t => phantom_send {
        // SAFETY: delegating to the real libc send(2).
        let result = unsafe { redhook::real!(send)(sockfd, buf, len, flags) };
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    // SAFETY: buf points to `result` readable bytes (guaranteed by send contract).
                    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, result as usize) };
                    process_outgoing(sockfd as usize, data, false);
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
        // SAFETY: delegating to the real libc recv(2).
        let result = unsafe { redhook::real!(recv)(sockfd, buf, len, flags) };
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    // SAFETY: buf holds `result` initialised bytes written by recv(2).
                    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, result as usize) };
                    process_incoming(sockfd as usize, data);
                    g.set(false);
                }
            });
        }
        result
    }
}

redhook::hook! {
    unsafe fn close(fd: c_int) -> c_int => phantom_close {
        // SAFETY: delegating to the real libc close(2).
        let result = unsafe { redhook::real!(close)(fd) };
        IN_HOOK.with(|g| {
            if !g.get() {
                g.set(true);
                process_teardown(fd as usize);
                g.set(false);
            }
        });
        result
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Hooks — OpenSSL / LibreSSL / BoringSSL (HTTPS)
//
// SSL_write / SSL_read operate on plaintext above the TLS layer, so we can
// capture the decrypted HTTP traffic. SSL_free cleans up on connection close.
//
// The IN_HOOK guard prevents double-capture: when SSL_write internally calls
// send(), the send hook sees IN_HOOK=true and skips.
// ─────────────────────────────────────────────────────────────────────────────

redhook::hook! {
    unsafe fn SSL_write(
        ssl: *mut c_void,
        buf: *const c_void,
        num: c_int
    ) -> c_int => phantom_ssl_write {
        let result = unsafe { redhook::real!(SSL_write)(ssl, buf, num) };
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    // SAFETY: buf points to `result` bytes that were written successfully.
                    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, result as usize) };
                    process_outgoing(ssl as usize, data, true);
                    g.set(false);
                }
            });
        }
        result
    }
}

redhook::hook! {
    unsafe fn SSL_read(
        ssl: *mut c_void,
        buf: *mut c_void,
        num: c_int
    ) -> c_int => phantom_ssl_read {
        let result = unsafe { redhook::real!(SSL_read)(ssl, buf, num) };
        if result > 0 {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    // SAFETY: buf holds `result` decrypted bytes from SSL_read.
                    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, result as usize) };
                    process_incoming(ssl as usize, data);
                    g.set(false);
                }
            });
        }
        result
    }
}

redhook::hook! {
    unsafe fn SSL_free(ssl: *mut c_void) => phantom_ssl_free {
        // Emit any buffered partial response before freeing the SSL context.
        IN_HOOK.with(|g| {
            if !g.get() {
                g.set(true);
                process_teardown(ssl as usize);
                g.set(false);
            }
        });
        // SAFETY: delegating to the real SSL_free.
        unsafe { redhook::real!(SSL_free)(ssl) }
    }
}
