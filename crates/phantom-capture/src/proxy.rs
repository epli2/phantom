use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use http::uri::Scheme;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::hyper::{Request, Response};
use hudsucker::rcgen::{CertificateParams, KeyPair};
use hudsucker::{Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use phantom_core::capture::CaptureBackend;
use phantom_core::error::CaptureError;
use phantom_core::redact::{self, RedactionConfig};
use phantom_core::trace::{HttpMethod, HttpTrace, SpanId, TraceId};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::fault::{FaultConfig, FaultRule};

/// Maximum body size to capture (1 MB).
const MAX_BODY_SIZE: usize = 1024 * 1024;

/// File names of the persistent CA material inside the CA directory.
const CA_KEY_FILE: &str = "phantom-ca.key.pem";
const CA_CERT_FILE: &str = "phantom-ca.cert.pem";

pub struct ProxyCaptureBackend {
    listen_port: u16,
    insecure: bool,
    ca_dir: Option<PathBuf>,
    fault_config: FaultConfig,
    max_body_size: usize,
    redaction: RedactionConfig,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ProxyCaptureBackend {
    pub fn new(listen_port: u16, insecure: bool) -> Self {
        Self {
            listen_port,
            insecure,
            ca_dir: None,
            fault_config: FaultConfig::default(),
            max_body_size: MAX_BODY_SIZE,
            redaction: RedactionConfig::default(),
            shutdown_tx: None,
            task_handle: None,
        }
    }

    /// Attach fault injection rules (builder pattern).
    pub fn with_faults(mut self, config: FaultConfig) -> Self {
        self.fault_config = config;
        self
    }

    /// Persist the MITM CA under `dir` so the same CA is reused across runs
    /// (builder pattern). Without this the CA is ephemeral and regenerated on
    /// every start, which forces clients to disable TLS verification.
    pub fn with_ca_dir(mut self, dir: PathBuf) -> Self {
        self.ca_dir = Some(dir);
        self
    }

    /// Set the maximum body size to capture, in bytes (builder pattern).
    /// `0` means unlimited. Defaults to 1 MiB.
    pub fn with_max_body_size(mut self, max_body_size: usize) -> Self {
        self.max_body_size = max_body_size;
        self
    }

    /// Redact configured header values and JSON body fields before a trace
    /// is emitted (builder pattern). Empty by default (no redaction).
    pub fn with_redaction(mut self, redaction: RedactionConfig) -> Self {
        self.redaction = redaction;
        self
    }
}

impl CaptureBackend for ProxyCaptureBackend {
    fn start(&mut self) -> Result<mpsc::Receiver<HttpTrace>, CaptureError> {
        let (trace_tx, trace_rx) = mpsc::channel(4096);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handler = TraceHandler {
            trace_tx,
            pending: None,
            fault_config: Arc::new(self.fault_config.clone()),
            max_body_size: self.max_body_size,
            redaction: Arc::new(self.redaction.clone()),
        };

        let port = self.listen_port;
        let insecure = self.insecure;
        let ca_dir = self.ca_dir.clone();

        let task_handle = tokio::spawn(async move {
            let (key_pair, ca_cert) = match &ca_dir {
                Some(dir) => load_or_generate_ca(dir),
                None => generate_ca(),
            };
            let ca = RcgenAuthority::new(key_pair, ca_cert, 1000);

            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            info!("Starting proxy on {addr}");

            if insecure {
                info!("TLS verification disabled (--insecure)");
                let client = build_insecure_client();
                let proxy = Proxy::builder()
                    .with_addr(addr)
                    .with_client(client)
                    .with_ca(ca)
                    .with_http_handler(handler)
                    .with_graceful_shutdown(async {
                        shutdown_rx.await.ok();
                    })
                    .build();
                if let Err(e) = proxy.start().await {
                    warn!("Proxy error: {e}");
                }
            } else {
                let proxy = Proxy::builder()
                    .with_addr(addr)
                    .with_rustls_client()
                    .with_ca(ca)
                    .with_http_handler(handler)
                    .with_graceful_shutdown(async {
                        shutdown_rx.await.ok();
                    })
                    .build();
                if let Err(e) = proxy.start().await {
                    warn!("Proxy error: {e}");
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
        Ok(())
    }

    fn name(&self) -> &str {
        "proxy"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CA management
// ─────────────────────────────────────────────────────────────────────────────

/// Paths of the persistent CA material managed by [`ensure_ca`].
#[derive(Debug, Clone)]
pub struct CaPaths {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

/// Ensure a persistent CA exists under `ca_dir`, generating it on first use,
/// and return the file paths. Safe to call repeatedly; the CA is only
/// regenerated when the key file is missing or unreadable.
pub fn ensure_ca(ca_dir: &Path) -> Result<CaPaths, CaptureError> {
    try_load_or_generate_ca(ca_dir)?;
    Ok(CaPaths {
        cert_path: ca_dir.join(CA_CERT_FILE),
        key_path: ca_dir.join(CA_KEY_FILE),
    })
}

fn build_ca_params() -> CertificateParams {
    use hudsucker::rcgen;

    let mut params = CertificateParams::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Phantom Proxy CA");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Phantom");
    params
}

/// Generate an ephemeral self-signed CA certificate for HTTPS interception.
/// Used when no CA directory is configured (e.g. library use in tests).
fn generate_ca() -> (KeyPair, hudsucker::rcgen::Certificate) {
    let key_pair = KeyPair::generate().expect("Failed to generate CA key pair");
    let ca_cert = build_ca_params()
        .self_signed(&key_pair)
        .expect("Failed to self-sign CA certificate");

    (key_pair, ca_cert)
}

/// Load the persistent CA from `ca_dir`, generating it on first use.
/// Falls back to an ephemeral CA if the directory is unusable, so a broken
/// filesystem never prevents the proxy from starting.
fn load_or_generate_ca(ca_dir: &Path) -> (KeyPair, hudsucker::rcgen::Certificate) {
    match try_load_or_generate_ca(ca_dir) {
        Ok(pair) => pair,
        Err(e) => {
            warn!(
                "failed to persist CA in {}: {e}; falling back to ephemeral CA",
                ca_dir.display()
            );
            generate_ca()
        }
    }
}

fn try_load_or_generate_ca(
    ca_dir: &Path,
) -> Result<(KeyPair, hudsucker::rcgen::Certificate), CaptureError> {
    let io_err = |e: std::io::Error| CaptureError::StartFailed(format!("CA storage: {e}"));

    std::fs::create_dir_all(ca_dir).map_err(io_err)?;
    let key_path = ca_dir.join(CA_KEY_FILE);
    let cert_path = ca_dir.join(CA_CERT_FILE);

    // Keep the private key out of version control if the data dir lives
    // inside a repository.
    let gitignore = ca_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n").map_err(io_err)?;
    }

    // Load the key pair, regenerating from scratch when missing or corrupt.
    let (key_pair, fresh_key) = match std::fs::read_to_string(&key_path) {
        Ok(pem) => match KeyPair::from_pem(&pem) {
            Ok(kp) => (kp, false),
            Err(e) => {
                warn!(
                    "corrupt CA key at {}: {e}; regenerating CA",
                    key_path.display()
                );
                let kp = KeyPair::generate()
                    .map_err(|e| CaptureError::StartFailed(format!("CA keygen: {e}")))?;
                (kp, true)
            }
        },
        Err(_) => {
            let kp = KeyPair::generate()
                .map_err(|e| CaptureError::StartFailed(format!("CA keygen: {e}")))?;
            (kp, true)
        }
    };

    if fresh_key {
        std::fs::write(&key_path, key_pair.serialize_pem()).map_err(io_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(io_err)?;
        }
        // A cert belonging to a previous key can no longer act as the issuer.
        let _ = std::fs::remove_file(&cert_path);
    }

    // Re-sign the CA params with the loaded key on every startup. Trust
    // anchors are matched by subject DN + public key, so the cert PEM written
    // on first run remains a valid anchor for leaf certs signed with this key
    // even though this in-memory certificate object is freshly signed.
    let ca_cert = build_ca_params()
        .self_signed(&key_pair)
        .map_err(|e| CaptureError::StartFailed(format!("CA self-sign: {e}")))?;

    if !cert_path.exists() {
        std::fs::write(&cert_path, ca_cert.pem()).map_err(io_err)?;
    }

    Ok((key_pair, ca_cert))
}

/// Handler is cloned per-connection by hudsucker. Within a single connection,
/// `handle_request` is always called before the corresponding `handle_response`,
/// so we store the pending request info directly on `self`.
#[derive(Clone)]
struct TraceHandler {
    trace_tx: mpsc::Sender<HttpTrace>,
    /// Pending request info, set in handle_request, consumed in handle_response.
    pending: Option<PendingRequest>,
    fault_config: Arc<FaultConfig>,
    max_body_size: usize,
    redaction: Arc<RedactionConfig>,
}

#[derive(Clone)]
struct PendingRequest {
    method: HttpMethod,
    url: String,
    request_headers: HashMap<String, String>,
    request_body: Option<Vec<u8>>,
    request_content_encoding: Option<String>,
    request_body_truncated: bool,
    request_body_binary: bool,
    source_addr: Option<String>,
    timestamp: SystemTime,
    started_at: Instant,
    span_id: SpanId,
    trace_id: TraceId,
    protocol_version: String,
}

impl HttpHandler for TraceHandler {
    async fn handle_request(&mut self, ctx: &HttpContext, req: Request<Body>) -> RequestOrResponse {
        let method = parse_method(req.method());
        let url = reconstruct_url(&req);
        let version = format!("{:?}", req.version());
        let headers = extract_headers(req.headers());
        let content_encoding = headers.get("content-encoding").cloned();

        let (parts, body) = req.into_parts();
        let collected = collect_body(body, self.max_body_size).await;
        let body_result = process_body(
            collected.recorded,
            content_encoding.as_deref(),
            self.max_body_size,
        );

        self.pending = Some(PendingRequest {
            method,
            url,
            request_headers: headers,
            request_body: body_result.data,
            request_content_encoding: body_result.content_encoding,
            request_body_truncated: collected.truncated || body_result.truncated,
            request_body_binary: body_result.is_binary,
            source_addr: Some(ctx.client_addr.to_string()),
            timestamp: SystemTime::now(),
            started_at: Instant::now(),
            span_id: SpanId(rand_bytes::<8>()),
            trace_id: TraceId(rand_bytes::<16>()),
            protocol_version: version,
        });

        // The upstream request must be forwarded byte-for-byte, independent of
        // any truncation/decoding applied to the recorded copy above.
        let rebuilt = Request::from_parts(parts, body_to_body(collected.wire_bytes));

        // Apply fault injection rules in order.
        let url = self
            .pending
            .as_ref()
            .map(|p| p.url.clone())
            .unwrap_or_default();
        for rule in &self.fault_config.rules {
            if !rule.matches_url(&url) {
                continue;
            }
            match rule {
                FaultRule::Delay { min_ms, max_ms, .. } => {
                    let delay_ms = if min_ms == max_ms {
                        *min_ms
                    } else {
                        min_ms + rand::random::<u64>() % (max_ms - min_ms + 1)
                    };
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                FaultRule::Error {
                    status_code,
                    probability,
                    ..
                } => {
                    if rand::random::<f64>() < *probability {
                        // Emit a trace immediately — handle_response won't be called.
                        if let Some(info) = self.pending.take() {
                            let fault_body = b"{\"fault\":\"injected\"}".to_vec();
                            let mut trace = HttpTrace {
                                span_id: info.span_id,
                                trace_id: info.trace_id,
                                parent_span_id: None,
                                method: info.method,
                                url: info.url,
                                request_headers: info.request_headers,
                                request_body: info.request_body,
                                request_content_encoding: info.request_content_encoding,
                                request_body_truncated: info.request_body_truncated,
                                request_body_binary: info.request_body_binary,
                                status_code: *status_code,
                                response_headers: {
                                    let mut h = HashMap::new();
                                    h.insert(
                                        "content-type".to_string(),
                                        "application/json".to_string(),
                                    );
                                    h.insert("x-fault-injected".to_string(), "phantom".to_string());
                                    h
                                },
                                response_body: Some(fault_body),
                                response_content_encoding: None,
                                response_body_truncated: false,
                                response_body_binary: false,
                                timestamp: info.timestamp,
                                duration: info.started_at.elapsed(),
                                source_addr: info.source_addr,
                                dest_addr: None,
                                protocol_version: info.protocol_version,
                            };
                            redact::redact_trace(&mut trace, &self.redaction);
                            if self.trace_tx.try_send(trace).is_err() {
                                warn!("Trace channel full, dropping fault-injected trace");
                            }
                        }
                        let body_bytes: bytes::Bytes = b"{\"fault\":\"injected\"}".as_ref().into();
                        let response = Response::builder()
                            .status(*status_code)
                            .header("content-type", "application/json")
                            .header("x-fault-injected", "phantom")
                            .body(Body::from(http_body_util::Full::new(body_bytes)))
                            .expect("valid fault response");
                        return RequestOrResponse::Response(response);
                    }
                }
            }
        }

        RequestOrResponse::Request(rebuilt)
    }

    async fn handle_response(&mut self, _ctx: &HttpContext, res: Response<Body>) -> Response<Body> {
        let (parts, body) = res.into_parts();
        let response_headers = extract_headers(&parts.headers);
        let content_encoding = response_headers.get("content-encoding").cloned();
        let status_code = parts.status.as_u16();
        let collected = collect_body(body, self.max_body_size).await;
        let body_result = process_body(
            collected.recorded,
            content_encoding.as_deref(),
            self.max_body_size,
        );

        // The downstream response must be forwarded byte-for-byte (still
        // compressed, never truncated), independent of the recorded copy.
        let rebuilt = Response::from_parts(parts, body_to_body(collected.wire_bytes));

        if let Some(info) = self.pending.take() {
            let duration = info.started_at.elapsed();
            let mut trace = HttpTrace {
                span_id: info.span_id,
                trace_id: info.trace_id,
                parent_span_id: None,
                method: info.method,
                url: info.url,
                request_headers: info.request_headers,
                request_body: info.request_body,
                request_content_encoding: info.request_content_encoding,
                request_body_truncated: info.request_body_truncated,
                request_body_binary: info.request_body_binary,
                status_code,
                response_headers,
                response_body: body_result.data,
                response_content_encoding: body_result.content_encoding,
                response_body_truncated: collected.truncated || body_result.truncated,
                response_body_binary: body_result.is_binary,
                timestamp: info.timestamp,
                duration,
                source_addr: info.source_addr,
                dest_addr: None,
                protocol_version: info.protocol_version,
            };
            redact::redact_trace(&mut trace, &self.redaction);

            if self.trace_tx.try_send(trace).is_err() {
                warn!("Trace channel full, dropping trace");
            }
        }

        rebuilt
    }
}

fn parse_method(method: &http::Method) -> HttpMethod {
    match method.as_str() {
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

fn reconstruct_url(req: &Request<Body>) -> String {
    let uri = req.uri();
    if uri.scheme().is_some() {
        return uri.to_string();
    }
    // For proxy requests the URI may be just a path; reconstruct from Host header.
    let scheme = if uri.scheme() == Some(&Scheme::HTTPS) {
        "https"
    } else {
        "http"
    };
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown");
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    format!("{scheme}://{host}{path}")
}

fn extract_headers(headers: &http::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect()
}

/// Wire-exact bytes plus a (possibly truncated) copy to keep for the trace.
struct CollectedBody {
    /// Complete, unmodified bytes. Always used to rebuild the request/response
    /// that continues on to the real destination or back to the client — the
    /// wire is never altered by recording, truncation, or decoding.
    wire_bytes: bytes::Bytes,
    /// Bytes kept for the trace, truncated at `max_body_size` (still encoded
    /// exactly as received on the wire; decoding happens in [`process_body`]).
    recorded: Option<Vec<u8>>,
    /// Whether `recorded` is a truncated prefix of the real body.
    truncated: bool,
}

/// Collect a full body from the wire. `max_body_size` bounds only the copy
/// kept for the trace (`0` means unlimited); the bytes forwarded onward are
/// always complete.
async fn collect_body(body: Body, max_body_size: usize) -> CollectedBody {
    use http_body_util::BodyExt;
    let wire_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => bytes::Bytes::new(),
    };

    if wire_bytes.is_empty() {
        return CollectedBody {
            wire_bytes,
            recorded: None,
            truncated: false,
        };
    }

    let (recorded, truncated) = if max_body_size > 0 && wire_bytes.len() > max_body_size {
        (wire_bytes[..max_body_size].to_vec(), true)
    } else {
        (wire_bytes.to_vec(), false)
    };

    CollectedBody {
        wire_bytes,
        recorded: Some(recorded),
        truncated,
    }
}

/// Outcome of decoding and classifying a recorded body copy.
struct BodyResult {
    data: Option<Vec<u8>>,
    /// Original `Content-Encoding` value, if the body was successfully decoded.
    content_encoding: Option<String>,
    /// Set if the decoded body had to be cut off at `max_body_size` (e.g. a
    /// small compressed body that decompresses to something much larger).
    truncated: bool,
    is_binary: bool,
}

/// Decode `recorded` per the response/request `Content-Encoding` header (if
/// present and not `identity`), then re-apply `max_body_size` to the decoded
/// bytes — decompression can expand well past the original wire size — and
/// classify the result as text or binary.
fn process_body(
    recorded: Option<Vec<u8>>,
    content_encoding_header: Option<&str>,
    max_body_size: usize,
) -> BodyResult {
    let Some(raw) = recorded else {
        return BodyResult {
            data: None,
            content_encoding: None,
            truncated: false,
            is_binary: false,
        };
    };

    let (mut data, content_encoding) = match content_encoding_header.map(str::trim) {
        Some(enc) if !enc.is_empty() && !enc.eq_ignore_ascii_case("identity") => {
            match decode_body(enc, &raw) {
                Some(decoded) => (decoded, Some(enc.to_string())),
                None => {
                    warn!("failed to decode Content-Encoding {enc:?}; storing raw body");
                    (raw, None)
                }
            }
        }
        _ => (raw, None),
    };

    let mut truncated = false;
    if max_body_size > 0 && data.len() > max_body_size {
        data.truncate(max_body_size);
        truncated = true;
    }

    let is_binary = is_binary_body(&data);

    BodyResult {
        data: Some(data),
        content_encoding,
        truncated,
        is_binary,
    }
}

/// Decode one `Content-Encoding` compression scheme.
fn decode_one(encoding: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    match encoding.to_ascii_lowercase().as_str() {
        "gzip" | "x-gzip" => {
            use std::io::Read;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .read_to_end(&mut out)
                .ok()?;
            Some(out)
        }
        "deflate" => {
            use std::io::Read;
            // "deflate" is ambiguous in the wild: most servers send a
            // zlib-wrapped stream, some send raw DEFLATE. Try zlib first.
            let mut out = Vec::new();
            if flate2::read::ZlibDecoder::new(bytes)
                .read_to_end(&mut out)
                .is_ok()
            {
                return Some(out);
            }
            out.clear();
            flate2::read::DeflateDecoder::new(bytes)
                .read_to_end(&mut out)
                .ok()?;
            Some(out)
        }
        "br" => {
            let mut out = Vec::new();
            brotli::BrotliDecompress(&mut std::io::Cursor::new(bytes), &mut out).ok()?;
            Some(out)
        }
        "zstd" => zstd::stream::decode_all(bytes).ok(),
        "identity" => Some(bytes.to_vec()),
        _ => None,
    }
}

/// Decode a `Content-Encoding` chain (e.g. `"gzip, br"`), applied right to
/// left per HTTP semantics (the last-listed encoding was applied first).
/// Returns `None` if any step fails, so the caller can fall back to the raw
/// bytes rather than corrupting or losing the body.
fn decode_body(encoding_header: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let tokens: Vec<&str> = encoding_header
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let mut current = bytes.to_vec();
    for enc in tokens.iter().rev() {
        current = decode_one(enc, &current)?;
    }
    Some(current)
}

/// Heuristic: a body counts as binary if its first 8 KB contain a NUL byte,
/// or more than 10% of that sample fails to decode as valid UTF-8.
fn is_binary_body(bytes: &[u8]) -> bool {
    let sample_len = bytes.len().min(8192);
    let sample = &bytes[..sample_len];
    if sample_len == 0 {
        return false;
    }
    if sample.contains(&0u8) {
        return true;
    }
    if std::str::from_utf8(sample).is_ok() {
        return false;
    }
    let lossy = String::from_utf8_lossy(sample);
    let replacement_count = lossy.matches('\u{FFFD}').count();
    (replacement_count as f64 / sample_len as f64) > 0.10
}

fn body_to_body(data: bytes::Bytes) -> Body {
    Body::from(http_body_util::Full::new(data))
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    buf.iter_mut().for_each(|b| *b = rand::random());
    buf
}

// ─────────────────────────────────────────────────────────────────────────────
// Insecure TLS client (--insecure mode)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a hyper client that skips all TLS certificate verification.
/// Used with `--insecure` for testing against backends with self-signed certs.
fn build_insecure_client() -> hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Body,
> {
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoCertVerifier))
        .with_no_client_auth();

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .build();

    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new()).build(https)
}

/// A [`rustls::client::danger::ServerCertVerifier`] that accepts any certificate.
/// For testing only — never use in production.
#[derive(Debug)]
struct NoCertVerifier;

impl rustls::client::danger::ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_ca_creates_files_on_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("ca");

        let paths = ensure_ca(&ca_dir).unwrap();

        assert!(paths.cert_path.exists(), "cert file created");
        assert!(paths.key_path.exists(), "key file created");
        assert!(ca_dir.join(".gitignore").exists(), ".gitignore created");

        let cert_pem = std::fs::read_to_string(&paths.cert_path).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&paths.key_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "key file is private");
        }
    }

    #[test]
    fn test_ensure_ca_is_stable_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("ca");

        let paths = ensure_ca(&ca_dir).unwrap();
        let cert_before = std::fs::read(&paths.cert_path).unwrap();
        let key_before = std::fs::read(&paths.key_path).unwrap();

        // Second load must reuse the same key and keep the cert file untouched.
        let (key_pair, _cert) = try_load_or_generate_ca(&ca_dir).unwrap();
        assert_eq!(std::fs::read(&paths.cert_path).unwrap(), cert_before);
        assert_eq!(std::fs::read(&paths.key_path).unwrap(), key_before);
        assert_eq!(
            key_pair.serialize_pem().as_bytes(),
            key_before.as_slice(),
            "loaded key matches the persisted key"
        );
    }

    #[test]
    fn test_ensure_ca_regenerates_on_corrupt_key() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("ca");

        let paths = ensure_ca(&ca_dir).unwrap();
        let cert_before = std::fs::read(&paths.cert_path).unwrap();
        std::fs::write(&paths.key_path, "not a pem").unwrap();

        let regenerated = ensure_ca(&ca_dir).unwrap();
        assert!(regenerated.cert_path.exists());
        assert!(regenerated.key_path.exists());
        let key_after = std::fs::read_to_string(&regenerated.key_path).unwrap();
        assert!(key_after.contains("BEGIN PRIVATE KEY"), "key regenerated");
        assert_ne!(
            std::fs::read(&regenerated.cert_path).unwrap(),
            cert_before,
            "cert rewritten for the new key"
        );
    }

    // ── Content-Encoding decoding ────────────────────────────────────────

    #[test]
    fn test_decode_body_gzip_roundtrip() {
        use std::io::Write;
        let original = b"{\"hello\":\"world\"}".to_vec();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let decoded = decode_body("gzip", &compressed).expect("gzip decode should succeed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_body_corrupt_gzip_does_not_panic() {
        let garbage = b"this is not gzip data at all".to_vec();
        assert!(decode_body("gzip", &garbage).is_none());
    }

    #[test]
    fn test_decode_body_zstd_roundtrip() {
        let original = b"the quick brown fox jumps over the lazy dog".to_vec();
        let compressed = zstd::stream::encode_all(&original[..], 0).unwrap();
        let decoded = decode_body("zstd", &compressed).expect("zstd decode should succeed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_body_brotli_roundtrip() {
        let original = b"phantom brotli test payload".to_vec();
        let mut compressed = Vec::new();
        let params = brotli::enc::BrotliEncoderParams::default();
        brotli::BrotliCompress(
            &mut std::io::Cursor::new(&original),
            &mut compressed,
            &params,
        )
        .unwrap();
        let decoded = decode_body("br", &compressed).expect("brotli decode should succeed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_body_unknown_encoding_returns_none() {
        assert!(decode_body("compress", b"whatever").is_none());
    }

    #[test]
    fn test_process_body_truncates_after_decode() {
        // A small compressed payload that decompresses to something larger
        // than max_body_size must be truncated post-decode, not rejected.
        let original = vec![b'a'; 10_000];
        let compressed = zstd::stream::encode_all(&original[..], 0).unwrap();
        assert!(compressed.len() < 100, "fixture should compress well");

        let result = process_body(Some(compressed), Some("zstd"), 100);
        assert!(result.truncated, "decoded body should be marked truncated");
        assert_eq!(result.data.as_ref().unwrap().len(), 100);
        assert_eq!(result.content_encoding.as_deref(), Some("zstd"));
    }

    #[test]
    fn test_process_body_no_encoding_passes_through() {
        let result = process_body(Some(b"plain text".to_vec()), None, 1024);
        assert_eq!(result.data.as_deref(), Some(b"plain text".as_slice()));
        assert_eq!(result.content_encoding, None);
        assert!(!result.truncated);
        assert!(!result.is_binary);
    }

    #[test]
    fn test_process_body_identity_passes_through() {
        let result = process_body(Some(b"plain text".to_vec()), Some("identity"), 1024);
        assert_eq!(result.data.as_deref(), Some(b"plain text".as_slice()));
        assert_eq!(result.content_encoding, None);
    }

    // ── Binary detection ─────────────────────────────────────────────────

    #[test]
    fn test_is_binary_body_detects_nul_bytes() {
        assert!(is_binary_body(b"\x00\x01\x02binary"));
    }

    #[test]
    fn test_is_binary_body_detects_invalid_utf8() {
        let mostly_invalid: Vec<u8> = (0..200).map(|_| 0xFFu8).collect();
        assert!(is_binary_body(&mostly_invalid));
    }

    #[test]
    fn test_is_binary_body_allows_plain_text() {
        assert!(!is_binary_body("hello, world! こんにちは".as_bytes()));
    }

    #[test]
    fn test_is_binary_body_empty_is_not_binary() {
        assert!(!is_binary_body(b""));
    }
}
