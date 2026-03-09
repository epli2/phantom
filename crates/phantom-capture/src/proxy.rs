use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use http::uri::Scheme;
use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::hyper::{Request, Response};
use hudsucker::rcgen::{CertificateParams, KeyPair};
use hudsucker::{Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use phantom_core::capture::CaptureBackend;
use phantom_core::error::CaptureError;
use phantom_core::trace::{HttpMethod, HttpTrace, SpanId, TraceId};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::fault::{FaultConfig, FaultRule};

/// Maximum body size to capture (1 MB).
const MAX_BODY_SIZE: usize = 1024 * 1024;

pub struct ProxyCaptureBackend {
    listen_port: u16,
    insecure: bool,
    fault_config: FaultConfig,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl ProxyCaptureBackend {
    pub fn new(listen_port: u16, insecure: bool) -> Self {
        Self {
            listen_port,
            insecure,
            fault_config: FaultConfig::default(),
            shutdown_tx: None,
            task_handle: None,
        }
    }

    /// Attach fault injection rules (builder pattern).
    pub fn with_faults(mut self, config: FaultConfig) -> Self {
        self.fault_config = config;
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
        };

        let port = self.listen_port;
        let insecure = self.insecure;

        let task_handle = tokio::spawn(async move {
            let (key_pair, ca_cert) = generate_ca();
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

/// Generate a self-signed CA certificate for HTTPS interception.
fn generate_ca() -> (KeyPair, hudsucker::rcgen::Certificate) {
    use hudsucker::rcgen;

    let mut params = CertificateParams::default();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Phantom Proxy CA");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Phantom");

    let key_pair = KeyPair::generate().expect("Failed to generate CA key pair");
    let ca_cert = params
        .self_signed(&key_pair)
        .expect("Failed to self-sign CA certificate");

    (key_pair, ca_cert)
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
}

#[derive(Clone)]
struct PendingRequest {
    method: HttpMethod,
    url: String,
    request_headers: HashMap<String, String>,
    request_body: Option<Vec<u8>>,
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

        let (parts, body) = req.into_parts();
        let body_bytes = collect_body(body).await;

        self.pending = Some(PendingRequest {
            method,
            url,
            request_headers: headers,
            request_body: body_bytes.clone(),
            source_addr: Some(ctx.client_addr.to_string()),
            timestamp: SystemTime::now(),
            started_at: Instant::now(),
            span_id: SpanId(rand_bytes::<8>()),
            trace_id: TraceId(rand_bytes::<16>()),
            protocol_version: version,
        });

        let rebuilt = Request::from_parts(parts, body_to_body(body_bytes));

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
                FaultRule::Delay {
                    min_ms, max_ms, ..
                } => {
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
                            let trace = HttpTrace {
                                span_id: info.span_id,
                                trace_id: info.trace_id,
                                parent_span_id: None,
                                method: info.method,
                                url: info.url,
                                request_headers: info.request_headers,
                                request_body: info.request_body,
                                status_code: *status_code,
                                response_headers: HashMap::new(),
                                response_body: Some(fault_body),
                                timestamp: info.timestamp,
                                duration: info.started_at.elapsed(),
                                source_addr: info.source_addr,
                                dest_addr: None,
                                protocol_version: info.protocol_version,
                            };
                            if self.trace_tx.try_send(trace).is_err() {
                                warn!("Trace channel full, dropping fault-injected trace");
                            }
                        }
                        let body_bytes: bytes::Bytes =
                            b"{\"fault\":\"injected\"}".as_ref().into();
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
        let status_code = parts.status.as_u16();
        let response_body = collect_body(body).await;

        let rebuilt = Response::from_parts(parts, body_to_body(response_body.clone()));

        if let Some(info) = self.pending.take() {
            let duration = info.started_at.elapsed();
            let trace = HttpTrace {
                span_id: info.span_id,
                trace_id: info.trace_id,
                parent_span_id: None,
                method: info.method,
                url: info.url,
                request_headers: info.request_headers,
                request_body: info.request_body,
                status_code,
                response_headers,
                response_body,
                timestamp: info.timestamp,
                duration,
                source_addr: info.source_addr,
                dest_addr: None,
                protocol_version: info.protocol_version,
            };

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

async fn collect_body(body: Body) -> Option<Vec<u8>> {
    use http_body_util::BodyExt;
    match body.collect().await {
        Ok(collected) => {
            let bytes = collected.to_bytes();
            if bytes.is_empty() {
                None
            } else if bytes.len() > MAX_BODY_SIZE {
                // Truncate large bodies
                Some(bytes[..MAX_BODY_SIZE].to_vec())
            } else {
                Some(bytes.to_vec())
            }
        }
        Err(_) => None,
    }
}

fn body_to_body(data: Option<Vec<u8>>) -> Body {
    match data {
        Some(bytes) => {
            let b: bytes::Bytes = bytes.into();
            Body::from(http_body_util::Full::new(b))
        }
        None => Body::empty(),
    }
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
