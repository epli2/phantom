//! Phantom LD_PRELOAD agent — Linux only.
//!
//! Build as a `dylib` and inject with:
//!   `LD_PRELOAD=/path/to/libphantom_agent.so PHANTOM_SOCKET=/tmp/phantom.sock <cmd>`
//!
//! The agent hooks `send()` / `recv()` / `close()` from libc to intercept
//! plain-text HTTP/1.x traffic, and `SSL_write()` / `SSL_read()` / `SSL_free()`
//! from OpenSSL/LibreSSL/BoringSSL to intercept HTTPS traffic (plaintext above
//! the TLS layer). Both HTTP/1.x and HTTP/2 are captured. Captured traces are
//! sent as JSON datagrams over a Unix datagram socket to the phantom main process.
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
    protocol_version: String,
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
// HTTP/2 — constants, frame parsing, per-stream/connection state
// ─────────────────────────────────────────────────────────────────────────────

/// HTTP/2 client connection preface (RFC 7540 §3.5).
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
/// Size of an HTTP/2 frame header in bytes.
const H2_FRAME_HDR_LEN: usize = 9;
// Frame types we care about.
const H2_TYPE_DATA: u8 = 0x0;
const H2_TYPE_HEADERS: u8 = 0x1;
const H2_TYPE_CONTINUATION: u8 = 0x9;
// Frame flags.
const H2_FLAG_END_STREAM: u8 = 0x1;
const H2_FLAG_END_HEADERS: u8 = 0x4;
const H2_FLAG_PADDED: u8 = 0x8;
const H2_FLAG_PRIORITY: u8 = 0x20;

/// Parse the 9-byte HTTP/2 frame header.
/// Returns `(payload_len, frame_type, flags, stream_id)` or `None` if buf is too short.
fn parse_h2_frame_header(buf: &[u8]) -> Option<(usize, u8, u8, u32)> {
    if buf.len() < H2_FRAME_HDR_LEN {
        return None;
    }
    let payload_len = ((buf[0] as usize) << 16) | ((buf[1] as usize) << 8) | (buf[2] as usize);
    let frame_type = buf[3];
    let flags = buf[4];
    // Mask off the reserved R bit (bit 31).
    let stream_id = u32::from_be_bytes([buf[5] & 0x7f, buf[6], buf[7], buf[8]]);
    Some((payload_len, frame_type, flags, stream_id))
}

/// Return the `[start, end)` byte range of the header block fragment within a
/// HEADERS frame payload, stripping optional padding and priority bytes.
fn h2_header_block_range(payload: &[u8], flags: u8) -> (usize, usize) {
    let mut start = 0usize;
    let mut end = payload.len();

    if flags & H2_FLAG_PADDED != 0 {
        if payload.is_empty() {
            return (0, 0);
        }
        let pad_len = payload[0] as usize;
        start += 1;
        end = end.saturating_sub(pad_len);
    }
    if flags & H2_FLAG_PRIORITY != 0 {
        start += 5; // 4 bytes stream dependency + 1 byte weight
    }
    if start > end { (0, 0) } else { (start, end) }
}

/// Per-stream state for a single HTTP/2 request-response pair.
struct H2Stream {
    req_method: Option<String>,
    req_path: Option<String>,
    req_authority: Option<String>,
    req_scheme: Option<String>,
    req_headers: HashMap<String, String>,
    req_body: Vec<u8>,
    /// True once we have seen END_STREAM on the request side.
    req_done: bool,
    started_at: Instant,
    timestamp_ms: u64,
    resp_status: Option<u16>,
    resp_headers: HashMap<String, String>,
    resp_body: Vec<u8>,
    /// True once we have seen END_STREAM on the response side.
    resp_done: bool,
    tls: bool,
}

impl H2Stream {
    fn new(tls: bool) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            req_method: None,
            req_path: None,
            req_authority: None,
            req_scheme: None,
            req_headers: HashMap::new(),
            req_body: Vec::new(),
            req_done: false,
            started_at: Instant::now(),
            timestamp_ms: ts,
            resp_status: None,
            resp_headers: HashMap::new(),
            resp_body: Vec::new(),
            resp_done: false,
            tls,
        }
    }
}

/// Per-connection state for an HTTP/2 connection.
struct H2ConnState {
    tls: bool,
    /// Buffered outgoing (app→server) bytes not yet consumed into complete frames.
    send_buf: Vec<u8>,
    /// Buffered incoming (server→app) bytes not yet consumed into complete frames.
    recv_buf: Vec<u8>,
    /// HPACK decoder for client-sent request headers.
    send_hpack: hpack::Decoder<'static>,
    /// HPACK decoder for server-sent response headers.
    recv_hpack: hpack::Decoder<'static>,
    /// Active streams keyed by HTTP/2 stream ID.
    streams: HashMap<u32, H2Stream>,
    // CONTINUATION frame accumulation (send direction).
    send_cont_sid: Option<u32>,
    send_cont_buf: Vec<u8>,
    send_cont_end_stream: bool,
    // CONTINUATION frame accumulation (recv direction).
    recv_cont_sid: Option<u32>,
    recv_cont_buf: Vec<u8>,
    recv_cont_end_stream: bool,
}

impl H2ConnState {
    fn new(tls: bool) -> Self {
        Self {
            tls,
            send_buf: Vec::new(),
            recv_buf: Vec::new(),
            send_hpack: hpack::Decoder::new(),
            recv_hpack: hpack::Decoder::new(),
            streams: HashMap::new(),
            send_cont_sid: None,
            send_cont_buf: Vec::new(),
            send_cont_end_stream: false,
            recv_cont_sid: None,
            recv_cont_buf: Vec::new(),
            recv_cont_end_stream: false,
        }
    }
}

/// Apply decoded HPACK name-value pairs to a stream's request pseudo-headers and
/// regular headers.
fn apply_h2_request_headers(stream: &mut H2Stream, headers: Vec<(Vec<u8>, Vec<u8>)>) {
    for (name, value) in headers {
        let name = String::from_utf8_lossy(&name).into_owned();
        let value = String::from_utf8_lossy(&value).into_owned();
        match name.as_str() {
            ":method" => stream.req_method = Some(value),
            ":path" => stream.req_path = Some(value),
            ":scheme" => stream.req_scheme = Some(value),
            ":authority" => stream.req_authority = Some(value),
            n if !n.starts_with(':') => {
                stream.req_headers.insert(n.to_string(), value);
            }
            _ => {}
        }
    }
}

/// Apply decoded HPACK name-value pairs to a stream's response pseudo-headers and
/// regular headers.
fn apply_h2_response_headers(stream: &mut H2Stream, headers: Vec<(Vec<u8>, Vec<u8>)>) {
    for (name, value) in headers {
        let name = String::from_utf8_lossy(&name).into_owned();
        let value = String::from_utf8_lossy(&value).into_owned();
        if name == ":status" {
            stream.resp_status = value.parse().ok();
        } else if !name.starts_with(':') {
            stream.resp_headers.insert(name, value);
        }
    }
}

/// Process all complete HTTP/2 frames in `h2.send_buf` (outgoing / request side).
fn process_h2_send_frames(h2: &mut H2ConnState) {
    // Skip the 24-byte client connection preface if present at the start.
    if h2.send_buf.starts_with(H2_PREFACE) {
        h2.send_buf.drain(..H2_PREFACE.len());
    }

    loop {
        let Some((payload_len, frame_type, flags, stream_id)) = parse_h2_frame_header(&h2.send_buf)
        else {
            break;
        };
        let total = H2_FRAME_HDR_LEN + payload_len;
        if h2.send_buf.len() < total {
            break; // Frame not yet fully buffered.
        }

        // Clone payload so we can drain the buffer cleanly.
        let payload = h2.send_buf[H2_FRAME_HDR_LEN..total].to_vec();
        h2.send_buf.drain(..total);

        let tls = h2.tls;
        match frame_type {
            H2_TYPE_HEADERS if stream_id > 0 => {
                let end_stream = flags & H2_FLAG_END_STREAM != 0;
                let end_headers = flags & H2_FLAG_END_HEADERS != 0;
                let (hb_start, hb_end) = h2_header_block_range(&payload, flags);
                let hblock = &payload[hb_start..hb_end];

                if end_headers {
                    let decoded = h2.send_hpack.decode(hblock).unwrap_or_default();
                    let stream = h2
                        .streams
                        .entry(stream_id)
                        .or_insert_with(|| H2Stream::new(tls));
                    apply_h2_request_headers(stream, decoded);
                    stream.req_done |= end_stream;
                } else {
                    // Header block continues in CONTINUATION frames.
                    h2.send_cont_sid = Some(stream_id);
                    h2.send_cont_buf = hblock.to_vec();
                    h2.send_cont_end_stream = end_stream;
                }
            }
            H2_TYPE_DATA if stream_id > 0 => {
                let end_stream = flags & H2_FLAG_END_STREAM != 0;
                // Strip padding.
                let (data_start, data_end) = if flags & H2_FLAG_PADDED != 0 && !payload.is_empty() {
                    let pad = payload[0] as usize;
                    (1, payload.len().saturating_sub(pad))
                } else {
                    (0, payload.len())
                };
                if let Some(stream) = h2.streams.get_mut(&stream_id) {
                    if stream.req_body.len() < MAX_BUF {
                        stream
                            .req_body
                            .extend_from_slice(&payload[data_start..data_end]);
                    }
                    stream.req_done |= end_stream;
                }
            }
            H2_TYPE_CONTINUATION if stream_id > 0 => {
                if h2.send_cont_sid == Some(stream_id) {
                    h2.send_cont_buf.extend_from_slice(&payload);
                    if flags & H2_FLAG_END_HEADERS != 0 {
                        let hblock = std::mem::take(&mut h2.send_cont_buf);
                        let decoded = h2.send_hpack.decode(&hblock).unwrap_or_default();
                        let end_stream = h2.send_cont_end_stream;
                        let stream = h2
                            .streams
                            .entry(stream_id)
                            .or_insert_with(|| H2Stream::new(tls));
                        apply_h2_request_headers(stream, decoded);
                        stream.req_done |= end_stream;
                        h2.send_cont_sid = None;
                        h2.send_cont_end_stream = false;
                    }
                }
            }
            _ => {} // SETTINGS, WINDOW_UPDATE, PING, GOAWAY, etc. — ignore.
        }
    }
}

/// Process all complete HTTP/2 frames in `h2.recv_buf` (incoming / response side).
fn process_h2_recv_frames(h2: &mut H2ConnState) {
    loop {
        let Some((payload_len, frame_type, flags, stream_id)) = parse_h2_frame_header(&h2.recv_buf)
        else {
            break;
        };
        let total = H2_FRAME_HDR_LEN + payload_len;
        if h2.recv_buf.len() < total {
            break;
        }

        let payload = h2.recv_buf[H2_FRAME_HDR_LEN..total].to_vec();
        h2.recv_buf.drain(..total);

        let tls = h2.tls;
        match frame_type {
            H2_TYPE_HEADERS if stream_id > 0 => {
                let end_stream = flags & H2_FLAG_END_STREAM != 0;
                let end_headers = flags & H2_FLAG_END_HEADERS != 0;
                let (hb_start, hb_end) = h2_header_block_range(&payload, flags);
                let hblock = &payload[hb_start..hb_end];

                if end_headers {
                    let decoded = h2.recv_hpack.decode(hblock).unwrap_or_default();
                    let stream = h2
                        .streams
                        .entry(stream_id)
                        .or_insert_with(|| H2Stream::new(tls));
                    apply_h2_response_headers(stream, decoded);
                    stream.resp_done |= end_stream;
                } else {
                    h2.recv_cont_sid = Some(stream_id);
                    h2.recv_cont_buf = hblock.to_vec();
                    h2.recv_cont_end_stream = end_stream;
                }
            }
            H2_TYPE_DATA if stream_id > 0 => {
                let end_stream = flags & H2_FLAG_END_STREAM != 0;
                let (data_start, data_end) = if flags & H2_FLAG_PADDED != 0 && !payload.is_empty() {
                    let pad = payload[0] as usize;
                    (1, payload.len().saturating_sub(pad))
                } else {
                    (0, payload.len())
                };
                if let Some(stream) = h2.streams.get_mut(&stream_id) {
                    if stream.resp_body.len() < MAX_BUF {
                        stream
                            .resp_body
                            .extend_from_slice(&payload[data_start..data_end]);
                    }
                    stream.resp_done |= end_stream;
                }
            }
            H2_TYPE_CONTINUATION if stream_id > 0 => {
                if h2.recv_cont_sid == Some(stream_id) {
                    h2.recv_cont_buf.extend_from_slice(&payload);
                    if flags & H2_FLAG_END_HEADERS != 0 {
                        let hblock = std::mem::take(&mut h2.recv_cont_buf);
                        let decoded = h2.recv_hpack.decode(&hblock).unwrap_or_default();
                        let end_stream = h2.recv_cont_end_stream;
                        let stream = h2
                            .streams
                            .entry(stream_id)
                            .or_insert_with(|| H2Stream::new(tls));
                        apply_h2_response_headers(stream, decoded);
                        stream.resp_done |= end_stream;
                        h2.recv_cont_sid = None;
                        h2.recv_cont_end_stream = false;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Remove and return all streams that have a complete response (status + END_STREAM).
fn drain_completed_h2_streams(h2: &mut H2ConnState) -> Vec<H2Stream> {
    let done_ids: Vec<u32> = h2
        .streams
        .iter()
        .filter(|(_, s)| s.resp_status.is_some() && s.resp_done)
        .map(|(id, _)| *id)
        .collect();
    let mut completed = Vec::with_capacity(done_ids.len());
    for id in done_ids {
        if let Some(stream) = h2.streams.remove(&id) {
            completed.push(stream);
        }
    }
    completed
}

/// Build and send a `TraceMsg` for a completed HTTP/2 stream.
fn emit_h2_stream(stream: H2Stream) {
    let method = stream.req_method.unwrap_or_else(|| "GET".to_string());
    let path = stream.req_path.unwrap_or_else(|| "/".to_string());
    let authority = stream.req_authority.unwrap_or_default();
    let scheme = stream
        .req_scheme
        .unwrap_or_else(|| if stream.tls { "https" } else { "http" }.to_string());
    let url = format!("{scheme}://{authority}{path}");
    let status_code = stream.resp_status.unwrap_or(0);
    let duration = stream.started_at.elapsed();

    emit_msg(&TraceMsg {
        method,
        url,
        status_code,
        request_headers: stream.req_headers,
        response_headers: stream.resp_headers,
        request_body_b64: body_b64(&stream.req_body),
        response_body_b64: body_b64(&stream.resp_body),
        duration_ms: duration.as_millis() as u64,
        timestamp_ms: stream.timestamp_ms,
        dest_addr: None,
        protocol_version: "HTTP/2".to_string(),
    });
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
    CollectingRequest { buf: Vec<u8> },
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
    /// HTTP/2 connection (may carry many multiplexed streams).
    Http2(Box<H2ConnState>),
    /// MySQL connection — tracks COM_QUERY round-trips.
    MysqlConnection(Box<MysqlConnState>),
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
// Emit a completed HTTP/1.x trace
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
        protocol_version: "HTTP/1.1".to_string(),
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// MySQL wire protocol — state machine and helpers
// ─────────────────────────────────────────────────────────────────────────────

/// MySQL port to intercept (default 3306, override with PHANTOM_MYSQL_PORT).
static MYSQL_PORT: OnceLock<u16> = OnceLock::new();

fn mysql_port() -> u16 {
    *MYSQL_PORT.get_or_init(|| {
        std::env::var("PHANTOM_MYSQL_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3306)
    })
}

/// Parse a MySQL packet header: `[3B length LE][1B seq_id][payload]`.
/// Returns `(total_bytes_consumed, seq_id, payload_slice)` or `None` if incomplete.
fn parse_mysql_packet(buf: &[u8]) -> Option<(usize, u8, &[u8])> {
    if buf.len() < 4 {
        return None;
    }
    let payload_len = u32::from_le_bytes([buf[0], buf[1], buf[2], 0]) as usize;
    let total = 4 + payload_len;
    if buf.len() < total {
        return None;
    }
    Some((total, buf[3], &buf[4..total]))
}

/// Decode a MySQL length-encoded integer.
/// Returns `(value, bytes_consumed)` or `None` on insufficient data.
fn decode_lenenc_int(buf: &[u8]) -> Option<(u64, usize)> {
    match buf.first()? {
        n @ 0x00..=0xfb => Some((*n as u64, 1)),
        0xfc => {
            if buf.len() < 3 {
                return None;
            }
            Some((u16::from_le_bytes([buf[1], buf[2]]) as u64, 3))
        }
        0xfd => {
            if buf.len() < 4 {
                return None;
            }
            Some((u32::from_le_bytes([buf[1], buf[2], buf[3], 0]) as u64, 4))
        }
        0xfe => {
            if buf.len() < 9 {
                return None;
            }
            let bytes: [u8; 8] = buf[1..9].try_into().ok()?;
            Some((u64::from_le_bytes(bytes), 9))
        }
        _ => None, // 0xff = ERR indicator, not a length
    }
}

#[derive(Debug)]
enum HandshakePhase {
    /// Waiting for the server greeting (seq_id=0, payload[0]=0x0a).
    WaitingGreeting,
    /// Server greeting seen; waiting for the server's auth OK/ERR (seq_id=2, payload[0]=0x00).
    WaitingAuthOk,
    /// Handshake complete; ready to track COM_QUERY commands.
    Done,
}

#[derive(Debug)]
enum ResultSetPhase {
    /// Reading column definition packets.
    ReadingColumns { cols_seen: u64 },
    /// Column defs done (EOF seen); reading data rows.
    ReadingRows,
}

#[derive(Debug)]
enum MysqlQueryState {
    Idle,
    /// COM_QUERY sent; waiting for the first server response packet.
    AwaitingResponse {
        query: String,
        started_at: Instant,
        timestamp_ms: u64,
    },
    /// First server packet was a column count — reading result set.
    ReadingResultSet {
        query: String,
        started_at: Instant,
        timestamp_ms: u64,
        column_count: u64,
        row_count: u64,
        phase: ResultSetPhase,
    },
}

#[derive(Debug)]
struct MysqlConnState {
    dest_addr: Option<String>,
    db_name: Option<String>,
    /// Bytes buffered from the client (send direction).
    send_buf: Vec<u8>,
    /// Bytes buffered from the server (recv direction).
    recv_buf: Vec<u8>,
    handshake: HandshakePhase,
    query_state: MysqlQueryState,
}

impl MysqlConnState {
    fn new(dest_addr: Option<String>) -> Self {
        Self {
            dest_addr,
            db_name: None,
            send_buf: Vec::new(),
            recv_buf: Vec::new(),
            handshake: HandshakePhase::WaitingGreeting,
            query_state: MysqlQueryState::Idle,
        }
    }
}

/// IPC message for a completed MySQL query (JSON-serialised to the same socket as HTTP traces).
#[derive(serde::Serialize)]
struct MysqlTraceMsg {
    msg_type: &'static str, // always "mysql"
    query: String,
    duration_ms: u64,
    timestamp_ms: u64,
    dest_addr: Option<String>,
    db_name: Option<String>,
    // OK response fields
    affected_rows: Option<u64>,
    last_insert_id: Option<u64>,
    warnings: Option<u16>,
    // ResultSet response fields
    column_count: Option<u64>,
    row_count: Option<u64>,
    // ERR response fields
    error_code: Option<u16>,
    sql_state: Option<String>,
    error_message: Option<String>,
}

fn emit_mysql_msg(msg: &MysqlTraceMsg) {
    let Some((sock, path)) = ipc() else { return };
    let Ok(data) = serde_json::to_vec(msg) else {
        return;
    };
    if data.len() <= MAX_DATAGRAM {
        let _ = sock.send_to(&data, path);
    }
}

/// Process bytes arriving from the client (client → server direction) for a MySQL FD.
/// May transition `state.query_state` from `Idle` to `AwaitingResponse`.
fn process_mysql_outgoing(state: &mut MysqlConnState, data: &[u8]) {
    if state.send_buf.len() < MAX_BUF {
        state.send_buf.extend_from_slice(data);
    }

    loop {
        let Some((consumed, seq_id, payload)) = parse_mysql_packet(&state.send_buf) else {
            break;
        };
        // Only act on packets during the command phase (handshake done, seq_id=0).
        if matches!(state.handshake, HandshakePhase::Done)
            && seq_id == 0
            && payload.first() == Some(&0x03) // COM_QUERY
            && matches!(state.query_state, MysqlQueryState::Idle)
        {
            let query = String::from_utf8_lossy(&payload[1..]).into_owned();
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            state.query_state = MysqlQueryState::AwaitingResponse {
                query,
                started_at: Instant::now(),
                timestamp_ms: ts,
            };
        }
        state.send_buf.drain(..consumed);
    }
}

/// Process bytes arriving from the server (server → client direction) for a MySQL FD.
/// Returns a `MysqlTraceMsg` when a query round-trip is complete.
fn process_mysql_incoming(state: &mut MysqlConnState, data: &[u8]) -> Option<MysqlTraceMsg> {
    if state.recv_buf.len() < MAX_BUF {
        state.recv_buf.extend_from_slice(data);
    }

    loop {
        let Some((consumed, seq_id, payload)) = parse_mysql_packet(&state.recv_buf) else {
            break;
        };

        // ── Handshake phase ──────────────────────────────────────────────────
        match &state.handshake {
            HandshakePhase::WaitingGreeting => {
                // Server greeting: seq_id=0, payload[0]=0x0a (protocol v10).
                if seq_id == 0 && payload.first() == Some(&0x0a) {
                    state.handshake = HandshakePhase::WaitingAuthOk;
                }
                state.recv_buf.drain(..consumed);
                continue;
            }
            HandshakePhase::WaitingAuthOk => {
                // Auth OK: payload[0]=0x00 (OK packet) at seq_id >= 2.
                if seq_id >= 2 && payload.first() == Some(&0x00) {
                    state.handshake = HandshakePhase::Done;
                }
                state.recv_buf.drain(..consumed);
                continue;
            }
            HandshakePhase::Done => {}
        }

        // ── Command-phase server responses ───────────────────────────────────
        match &mut state.query_state {
            MysqlQueryState::Idle => {
                state.recv_buf.drain(..consumed);
                continue;
            }

            MysqlQueryState::AwaitingResponse {
                query,
                started_at,
                timestamp_ms,
            } => {
                let first = payload.first().copied().unwrap_or(0);
                match first {
                    // OK packet
                    0x00 => {
                        let duration = started_at.elapsed();
                        let ts = *timestamp_ms;
                        let (affected_rows, off1) = decode_lenenc_int(&payload[1..])
                            .map(|(v, o)| (v, o + 1))
                            .unwrap_or((0, 1));
                        let (last_insert_id, _) =
                            decode_lenenc_int(&payload[off1..]).unwrap_or((0, 1));
                        // status flags (2B) and warnings (2B) follow but we skip bounds checks for brevity
                        let warnings = if payload.len() >= off1 + 1 + 4 {
                            u16::from_le_bytes([payload[off1 + 1], payload[off1 + 2]])
                        } else {
                            0
                        };
                        let msg = MysqlTraceMsg {
                            msg_type: "mysql",
                            query: query.clone(),
                            duration_ms: duration.as_millis() as u64,
                            timestamp_ms: ts,
                            dest_addr: state.dest_addr.clone(),
                            db_name: state.db_name.clone(),
                            affected_rows: Some(affected_rows),
                            last_insert_id: Some(last_insert_id),
                            warnings: Some(warnings),
                            column_count: None,
                            row_count: None,
                            error_code: None,
                            sql_state: None,
                            error_message: None,
                        };
                        state.query_state = MysqlQueryState::Idle;
                        state.recv_buf.drain(..consumed);
                        return Some(msg);
                    }
                    // ERR packet
                    0xff => {
                        let duration = started_at.elapsed();
                        let ts = *timestamp_ms;
                        let error_code = if payload.len() >= 3 {
                            u16::from_le_bytes([payload[1], payload[2]])
                        } else {
                            0
                        };
                        // sqlstate: '#' + 5 ASCII chars at payload[3..9], if present
                        let (sql_state, msg_start) = if payload.len() >= 9 && payload[3] == b'#' {
                            (String::from_utf8_lossy(&payload[4..9]).into_owned(), 9)
                        } else {
                            (String::new(), 3)
                        };
                        let error_message =
                            String::from_utf8_lossy(&payload[msg_start..]).into_owned();
                        let msg = MysqlTraceMsg {
                            msg_type: "mysql",
                            query: query.clone(),
                            duration_ms: duration.as_millis() as u64,
                            timestamp_ms: ts,
                            dest_addr: state.dest_addr.clone(),
                            db_name: state.db_name.clone(),
                            affected_rows: None,
                            last_insert_id: None,
                            warnings: None,
                            column_count: None,
                            row_count: None,
                            error_code: Some(error_code),
                            sql_state: Some(sql_state),
                            error_message: Some(error_message),
                        };
                        state.query_state = MysqlQueryState::Idle;
                        state.recv_buf.drain(..consumed);
                        return Some(msg);
                    }
                    // First packet of a ResultSet — byte is the column count (lenenc int).
                    _ => {
                        let column_count = decode_lenenc_int(payload).map(|(v, _)| v).unwrap_or(1);
                        let query = query.clone();
                        let started_at = *started_at;
                        let timestamp_ms = *timestamp_ms;
                        state.query_state = MysqlQueryState::ReadingResultSet {
                            query,
                            started_at,
                            timestamp_ms,
                            column_count,
                            row_count: 0,
                            phase: ResultSetPhase::ReadingColumns { cols_seen: 0 },
                        };
                        state.recv_buf.drain(..consumed);
                        continue;
                    }
                }
            }

            MysqlQueryState::ReadingResultSet {
                query,
                started_at,
                timestamp_ms,
                column_count,
                row_count,
                phase,
            } => {
                let first = payload.first().copied().unwrap_or(0);
                // EOF packet: payload[0]=0xfe AND payload length < 9 (to distinguish from lenenc 0xfe).
                let is_eof = first == 0xfe && payload.len() < 9;
                // OK terminator (CLIENT_DEPRECATE_EOF): 0x00 with seq_id > 1 in rows phase.
                let is_ok_terminator =
                    first == 0x00 && matches!(phase, ResultSetPhase::ReadingRows);

                match phase {
                    ResultSetPhase::ReadingColumns { cols_seen } => {
                        if is_eof {
                            // End of column definitions; start reading rows.
                            *phase = ResultSetPhase::ReadingRows;
                        } else {
                            *cols_seen += 1;
                        }
                        state.recv_buf.drain(..consumed);
                        continue;
                    }
                    ResultSetPhase::ReadingRows => {
                        if is_eof || is_ok_terminator {
                            // Result set complete.
                            let duration = started_at.elapsed();
                            let ts = *timestamp_ms;
                            let msg = MysqlTraceMsg {
                                msg_type: "mysql",
                                query: query.clone(),
                                duration_ms: duration.as_millis() as u64,
                                timestamp_ms: ts,
                                dest_addr: state.dest_addr.clone(),
                                db_name: state.db_name.clone(),
                                affected_rows: None,
                                last_insert_id: None,
                                warnings: None,
                                column_count: Some(*column_count),
                                row_count: Some(*row_count),
                                error_code: None,
                                sql_state: None,
                                error_message: None,
                            };
                            state.query_state = MysqlQueryState::Idle;
                            state.recv_buf.drain(..consumed);
                            return Some(msg);
                        } else if first == 0xff {
                            // ERR during result set streaming.
                            let error_code = if payload.len() >= 3 {
                                u16::from_le_bytes([payload[1], payload[2]])
                            } else {
                                0
                            };
                            let (sql_state, msg_start) = if payload.len() >= 9 && payload[3] == b'#'
                            {
                                (String::from_utf8_lossy(&payload[4..9]).into_owned(), 9)
                            } else {
                                (String::new(), 3)
                            };
                            let error_message =
                                String::from_utf8_lossy(&payload[msg_start..]).into_owned();
                            let duration = started_at.elapsed();
                            let ts = *timestamp_ms;
                            let msg = MysqlTraceMsg {
                                msg_type: "mysql",
                                query: query.clone(),
                                duration_ms: duration.as_millis() as u64,
                                timestamp_ms: ts,
                                dest_addr: state.dest_addr.clone(),
                                db_name: state.db_name.clone(),
                                affected_rows: None,
                                last_insert_id: None,
                                warnings: None,
                                column_count: Some(*column_count),
                                row_count: Some(*row_count),
                                error_code: Some(error_code),
                                sql_state: Some(sql_state),
                                error_message: Some(error_message),
                            };
                            state.query_state = MysqlQueryState::Idle;
                            state.recv_buf.drain(..consumed);
                            return Some(msg);
                        } else {
                            // Data row packet.
                            *row_count += 1;
                            state.recv_buf.drain(..consumed);
                            continue;
                        }
                    }
                }
            }
        }
    }

    None
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

    // ── MySQL path ───────────────────────────────────────────────────────────
    if let Some(FdState::MysqlConnection(state)) = map.get_mut(&key) {
        process_mysql_outgoing(state, data);
        return;
    }

    // ── HTTP/2 path ──────────────────────────────────────────────────────────
    // If we already know this connection is HTTP/2, route directly.
    if let Some(FdState::Http2(h2)) = map.get_mut(&key) {
        if h2.send_buf.len() < MAX_BUF {
            h2.send_buf.extend_from_slice(data);
        }
        process_h2_send_frames(h2);
        return;
    }
    // Detect a new HTTP/2 connection by its client preface.
    if data.starts_with(H2_PREFACE) {
        let mut h2 = Box::new(H2ConnState::new(tls));
        h2.send_buf.extend_from_slice(data);
        process_h2_send_frames(&mut h2);
        map.insert(key, FdState::Http2(h2));
        return;
    }

    // ── HTTP/1.x path ────────────────────────────────────────────────────────
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
            map.insert(key, FdState::CollectingRequest { buf });
        }
    } else {
        // Possible continuation of an incomplete request.
        let transition = if let Some(FdState::CollectingRequest { buf }) = map.get_mut(&key) {
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
    // ── MySQL path ───────────────────────────────────────────────────────────
    let mysql_msg = {
        let mut map = match state_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        if let Some(FdState::MysqlConnection(state)) = map.get_mut(&key) {
            process_mysql_incoming(state, data)
        } else {
            None
        }
    }; // lock released
    if let Some(msg) = mysql_msg {
        emit_mysql_msg(&msg);
        return;
    }

    // ── HTTP/2 path ──────────────────────────────────────────────────────────
    // Handle HTTP/2 streams, collecting those that have a complete response.
    // We release the lock before emitting.
    let h2_completed = {
        let mut map = match state_map().lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        if let Some(FdState::Http2(h2)) = map.get_mut(&key) {
            if h2.recv_buf.len() < MAX_BUF {
                h2.recv_buf.extend_from_slice(data);
            }
            process_h2_recv_frames(h2);
            Some(drain_completed_h2_streams(h2))
        } else {
            None
        }
    }; // lock released

    if let Some(completed) = h2_completed {
        for stream in completed {
            emit_h2_stream(stream);
        }
        return;
    }

    // ── HTTP/1.x path ────────────────────────────────────────────────────────
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

    match state {
        // HTTP/1.x: emit partial response (e.g. chunked or connection-close semantics).
        Some(FdState::CollectingResponse {
            req,
            buf,
            tls,
            status_code: Some(sc),
            resp_headers: Some(rh),
            content_length,
            headers_end: Some(he),
        }) => {
            let cl = content_length.unwrap_or_else(|| buf.len().saturating_sub(he));
            let body_end = (he + cl).min(buf.len());
            let duration = req.started_at.elapsed();
            do_emit(*req, sc, rh, &buf[he..body_end], duration, tls);
        }
        // HTTP/2: emit any streams for which we received at least a response status.
        Some(FdState::Http2(h2)) => {
            for (_sid, stream) in h2.streams {
                if stream.resp_status.is_some() {
                    emit_h2_stream(stream);
                }
            }
        }
        // MySQL: emit partial query trace if a query was in flight.
        Some(FdState::MysqlConnection(state)) => {
            let pending = match &state.query_state {
                MysqlQueryState::AwaitingResponse {
                    query,
                    started_at,
                    timestamp_ms,
                } => Some((query.clone(), *started_at, *timestamp_ms)),
                MysqlQueryState::ReadingResultSet {
                    query,
                    started_at,
                    timestamp_ms,
                    ..
                } => Some((query.clone(), *started_at, *timestamp_ms)),
                MysqlQueryState::Idle => None,
            };
            if let Some((query, started_at, timestamp_ms)) = pending {
                emit_mysql_msg(&MysqlTraceMsg {
                    msg_type: "mysql",
                    query,
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    timestamp_ms,
                    dest_addr: state.dest_addr.clone(),
                    db_name: state.db_name.clone(),
                    affected_rows: None,
                    last_insert_id: None,
                    warnings: None,
                    column_count: None,
                    row_count: None,
                    error_code: None,
                    sql_state: None,
                    error_message: None,
                });
            }
        }
        // If headers were never parsed, we have nothing useful to emit.
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Hooks — libc (plain HTTP + MySQL connect detection)
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the port from a `sockaddr` (supports AF_INET and AF_INET6).
/// Returns `None` for other address families.
unsafe fn extract_sockaddr_port(
    addr: *const libc::sockaddr,
    addrlen: libc::socklen_t,
) -> Option<u16> {
    if addr.is_null() {
        return None;
    }
    let family = unsafe { (*addr).sa_family } as i32;
    if family == libc::AF_INET && addrlen as usize >= std::mem::size_of::<libc::sockaddr_in>() {
        let sa = unsafe { &*(addr as *const libc::sockaddr_in) };
        return Some(u16::from_be(sa.sin_port));
    }
    if family == libc::AF_INET6 && addrlen as usize >= std::mem::size_of::<libc::sockaddr_in6>() {
        let sa = unsafe { &*(addr as *const libc::sockaddr_in6) };
        return Some(u16::from_be(sa.sin6_port));
    }
    None
}

/// Format a `sockaddr` as `"host:port"` string for MySQL dest_addr metadata.
unsafe fn format_sockaddr(addr: *const libc::sockaddr, addrlen: libc::socklen_t) -> Option<String> {
    if addr.is_null() {
        return None;
    }
    let family = unsafe { (*addr).sa_family } as i32;
    if family == libc::AF_INET && addrlen as usize >= std::mem::size_of::<libc::sockaddr_in>() {
        let sa = unsafe { &*(addr as *const libc::sockaddr_in) };
        let port = u16::from_be(sa.sin_port);
        let ip = u32::from_be(sa.sin_addr.s_addr);
        let a = (ip >> 24) & 0xff;
        let b = (ip >> 16) & 0xff;
        let c = (ip >> 8) & 0xff;
        let d = ip & 0xff;
        return Some(format!("{a}.{b}.{c}.{d}:{port}"));
    }
    None
}

redhook::hook! {
    unsafe fn connect(
        sockfd:  c_int,
        addr:    *const libc::sockaddr,
        addrlen: libc::socklen_t
    ) -> c_int => phantom_connect {
        // SAFETY: delegating to the real libc connect(2).
        let result = unsafe { redhook::real!(connect)(sockfd, addr, addrlen) };
        // result == 0 (success) or -1 with EINPROGRESS (non-blocking) — both count.
        let errno = unsafe { *libc::__errno_location() };
        if result == 0 || (result == -1 && errno == libc::EINPROGRESS) {
            IN_HOOK.with(|g| {
                if !g.get() {
                    g.set(true);
                    // SAFETY: addr lifetime is valid for the duration of connect(2).
                    let port = unsafe { extract_sockaddr_port(addr, addrlen) };
                    if port == Some(mysql_port()) {
                        let dest = unsafe { format_sockaddr(addr, addrlen) };
                        if let Ok(mut map) = state_map().lock() {
                            map.insert(
                                sockfd as usize,
                                FdState::MysqlConnection(Box::new(MysqlConnState::new(dest))),
                            );
                        }
                    }
                    g.set(false);
                }
            });
        }
        result
    }
}

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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_mysql_packet ────────────────────────────────────────────────────

    #[test]
    fn test_parse_mysql_packet_complete() {
        // 3-byte length (5) + seq_id (0) + payload "hello"
        let buf = [5u8, 0, 0, 0, b'h', b'e', b'l', b'l', b'o'];
        let (consumed, seq, payload) = parse_mysql_packet(&buf).unwrap();
        assert_eq!(consumed, 9);
        assert_eq!(seq, 0);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn test_parse_mysql_packet_incomplete() {
        // Says 10 bytes payload but buf only has 5
        let buf = [10u8, 0, 0, 0, b'h', b'i'];
        assert!(parse_mysql_packet(&buf).is_none());
    }

    #[test]
    fn test_parse_mysql_packet_too_short_for_header() {
        assert!(parse_mysql_packet(&[1, 0, 0]).is_none());
    }

    // ── decode_lenenc_int ─────────────────────────────────────────────────────

    #[test]
    fn test_decode_lenenc_int_1byte() {
        assert_eq!(decode_lenenc_int(&[42]), Some((42, 1)));
        assert_eq!(decode_lenenc_int(&[0]), Some((0, 1)));
        assert_eq!(decode_lenenc_int(&[0xfb]), Some((251, 1)));
    }

    #[test]
    fn test_decode_lenenc_int_2byte() {
        // 0xfc marker + little-endian u16
        let buf = [0xfc, 0x01, 0x00]; // = 1
        assert_eq!(decode_lenenc_int(&buf), Some((1, 3)));
        let buf = [0xfc, 0xff, 0xff]; // = 65535
        assert_eq!(decode_lenenc_int(&buf), Some((65535, 3)));
    }

    #[test]
    fn test_decode_lenenc_int_3byte() {
        let buf = [0xfd, 0x01, 0x00, 0x00]; // = 1
        assert_eq!(decode_lenenc_int(&buf), Some((1, 4)));
    }

    #[test]
    fn test_decode_lenenc_int_8byte() {
        let mut buf = [0u8; 9];
        buf[0] = 0xfe;
        buf[1] = 42;
        assert_eq!(decode_lenenc_int(&buf), Some((42, 9)));
    }

    #[test]
    fn test_decode_lenenc_int_insufficient() {
        assert_eq!(decode_lenenc_int(&[0xfc, 0x01]), None); // need 3 bytes
        assert_eq!(decode_lenenc_int(&[]), None);
    }

    // ── MySQL state machine ───────────────────────────────────────────────────

    /// Build a MySQL packet: [len 3B LE][seq_id 1B][payload].
    fn make_mysql_packet(seq_id: u8, payload: &[u8]) -> Vec<u8> {
        let len = payload.len() as u32;
        let mut v = vec![
            (len & 0xff) as u8,
            ((len >> 8) & 0xff) as u8,
            ((len >> 16) & 0xff) as u8,
            seq_id,
        ];
        v.extend_from_slice(payload);
        v
    }

    fn do_handshake(state: &mut MysqlConnState) {
        // Server greeting (seq=0, first byte 0x0a)
        let greeting = make_mysql_packet(0, &[0x0a, b'8', b'.', b'0', 0]);
        process_mysql_incoming(state, &greeting);
        // Client auth response (sent from client, ignored for handshake purposes)
        let auth = make_mysql_packet(1, &[0x00]);
        process_mysql_outgoing(state, &auth);
        // Server auth OK (seq=2, 0x00)
        let auth_ok = make_mysql_packet(2, &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
        process_mysql_incoming(state, &auth_ok);
        assert!(matches!(state.handshake, HandshakePhase::Done));
    }

    #[test]
    fn test_mysql_state_ok_response() {
        let mut state = MysqlConnState::new(Some("127.0.0.1:3306".to_string()));
        do_handshake(&mut state);

        // COM_QUERY: seq=0, 0x03 + SQL text
        let query_pkt = make_mysql_packet(0, b"\x03SELECT 1");
        process_mysql_outgoing(&mut state, &query_pkt);
        assert!(matches!(
            state.query_state,
            MysqlQueryState::AwaitingResponse { .. }
        ));

        // Server OK: seq=1, 0x00 + affected_rows(0) + last_insert_id(0) + status(2B) + warnings(2B)
        let ok_payload = [0x00u8, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
        let ok_pkt = make_mysql_packet(1, &ok_payload);
        let msg = process_mysql_incoming(&mut state, &ok_pkt).unwrap();
        assert_eq!(msg.query, "SELECT 1");
        assert_eq!(msg.affected_rows, Some(0));
        assert!(msg.error_code.is_none());
        assert!(matches!(state.query_state, MysqlQueryState::Idle));
    }

    #[test]
    fn test_mysql_state_err_response() {
        let mut state = MysqlConnState::new(None);
        do_handshake(&mut state);

        let query_pkt = make_mysql_packet(0, b"\x03BAD QUERY");
        process_mysql_outgoing(&mut state, &query_pkt);

        // ERR: 0xff + error_code(2B LE) + '#' + sqlstate(5B) + message
        let mut err_payload = vec![0xffu8, 0x28, 0x04]; // error_code = 0x0428 = 1064
        err_payload.extend_from_slice(b"#42000syntax error");
        let err_pkt = make_mysql_packet(1, &err_payload);
        let msg = process_mysql_incoming(&mut state, &err_pkt).unwrap();
        assert_eq!(msg.query, "BAD QUERY");
        assert_eq!(msg.error_code, Some(1064));
        assert_eq!(msg.sql_state.as_deref(), Some("42000"));
        assert!(
            msg.error_message
                .as_deref()
                .unwrap()
                .contains("syntax error")
        );
    }

    #[test]
    fn test_mysql_state_resultset() {
        let mut state = MysqlConnState::new(None);
        do_handshake(&mut state);

        // COM_QUERY
        let query_pkt = make_mysql_packet(0, b"\x03SELECT id, name FROM users");
        process_mysql_outgoing(&mut state, &query_pkt);

        // Column count packet: 2 columns (lenenc int = 0x02)
        let col_count_pkt = make_mysql_packet(1, &[0x02]);
        let r = process_mysql_incoming(&mut state, &col_count_pkt);
        assert!(r.is_none());
        assert!(matches!(
            state.query_state,
            MysqlQueryState::ReadingResultSet { .. }
        ));

        // Two column definition packets (arbitrary content, not 0xfe)
        for seq in 2u8..4 {
            let col_def = make_mysql_packet(seq, b"def\x00\x00\x00id");
            process_mysql_incoming(&mut state, &col_def);
        }

        // EOF after columns (payload < 9 bytes, first byte 0xfe)
        let eof_cols = make_mysql_packet(4, &[0xfe, 0x00, 0x00, 0x02, 0x00]);
        process_mysql_incoming(&mut state, &eof_cols);

        // Two data rows (not 0xfe)
        for seq in 5u8..7 {
            let row = make_mysql_packet(seq, b"\x011\x05Alice");
            process_mysql_incoming(&mut state, &row);
        }

        // EOF terminator
        let eof_rows = make_mysql_packet(7, &[0xfe, 0x00, 0x00, 0x02, 0x00]);
        let msg = process_mysql_incoming(&mut state, &eof_rows).unwrap();
        assert_eq!(msg.column_count, Some(2));
        assert_eq!(msg.row_count, Some(2));
        assert!(msg.error_code.is_none());
        assert!(matches!(state.query_state, MysqlQueryState::Idle));
    }
}
