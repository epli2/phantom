//! Integration test: phantom proxy transparently traces a PHP curl app
//!
//! Verifies non-invasive proxy tracing: the PHP client has ZERO proxy
//! awareness and ZERO TLS configuration. `phantom -- php client.php`
//! automatically:
//!   * sets HTTP_PROXY / HTTPS_PROXY (and lowercase variants), which
//!     libcurl honours natively without any `curl_setopt(CURLOPT_PROXY, ...)`
//!     call in the application, and
//!   * exports the MITM CA certificate to a temp PEM file and injects it via
//!     `-d curl.cainfo=<path>`, so curl's normal (non-bypassed) certificate
//!     verification trusts phantom's dynamically-generated leaf certs.
//!
//! Tests both HTTP and HTTPS (MITM) capture via the curl extension.
//!
//! Requirements: `php` (with the curl extension) on PATH.
//! Run: `cargo test --test proxy_php_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (mirrors proxy_node_integration.rs)
// ─────────────────────────────────────────────────────────────────────────────

fn available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind :0")
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock backend — HTTP
// ─────────────────────────────────────────────────────────────────────────────

const HEALTH_BODY: &str = r#"{"status":"ok"}"#;
const USERS_BODY: &str = r#"[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"}]"#;
const CREATED_BODY: &str = r#"{"id":3,"name":"Charlie","email":"charlie@example.com"}"#;

fn route_request(req: &str) -> (&str, &str) {
    let first = req.lines().next().unwrap_or("");
    if first.starts_with("GET") && first.contains("/api/health") {
        ("200 OK", HEALTH_BODY)
    } else if first.starts_with("GET") && first.contains("/api/users") {
        ("200 OK", USERS_BODY)
    } else if first.starts_with("POST") && first.contains("/api/users") {
        ("201 Created", CREATED_BODY)
    } else {
        ("404 Not Found", r#"{"error":"Not Found"}"#)
    }
}

fn write_response(stream: &mut impl IoWrite, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn handle_stream(stream: &mut (impl Read + IoWrite)) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let (status, body) = route_request(&req);
    write_response(stream, status, body);
}

/// Start a plain HTTP mock backend on `port`. Returns a join handle.
fn start_http_backend(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        listener
            .set_nonblocking(false)
            .expect("set_nonblocking(false)");
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => handle_stream(&mut s),
                Err(_) => break,
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Mock backend — HTTPS (rustls)
// ─────────────────────────────────────────────────────────────────────────────

fn start_https_backend(
    port: u16,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let certs = vec![rustls_pki_types::CertificateDer::from(cert_der)];
        let key = rustls_pki_types::PrivateKeyDer::try_from(key_der).expect("parse private key");

        let server_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .expect("build ServerConfig"),
        );

        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        for stream in listener.incoming() {
            match stream {
                Ok(tcp) => {
                    let conn = match rustls::ServerConnection::new(server_config.clone()) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let mut tls = rustls::StreamOwned::new(conn, tcp);
                    handle_stream(&mut tls);
                }
                Err(_) => break,
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_proxy_captures_php_curl_traffic() {
    // Pre-flight: php (with curl extension) available?
    let php_version = Command::new("php")
        .args(["-m"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match &php_version {
        Ok(out) if out.status.success() => {
            let modules = String::from_utf8_lossy(&out.stdout);
            if !modules.lines().any(|l| l.eq_ignore_ascii_case("curl")) {
                eprintln!("SKIP: PHP curl extension not loaded");
                return;
            }
        }
        _ => {
            eprintln!("SKIP: `php` not found");
            return;
        }
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/php-app");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let http_port = available_port();
    let https_port = available_port();
    let proxy_port = available_port();

    // ── Generate self-signed cert for the mock HTTPS backend ──────────────
    // Unrelated to phantom's own MITM CA: this is the "real" upstream server
    // cert that phantom's proxy connects to on the PHP client's behalf.
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    // ── Start HTTP backend ───────────────────────────────────────────────
    let _http_thread = start_http_backend(http_port);
    assert!(
        wait_for_port(http_port, Duration::from_secs(3)),
        "HTTP backend"
    );

    // ── Start HTTPS backend ──────────────────────────────────────────────
    let _https_thread = start_https_backend(https_port, cert_der, key_der);
    assert!(
        wait_for_port(https_port, Duration::from_secs(3)),
        "HTTPS backend"
    );

    // ── Run phantom with `-- php client.php` ─────────────────────────────
    // In JSONL mode phantom exits automatically when the child process exits.
    // --insecure only affects phantom's own connection to the (self-signed)
    // mock HTTPS backend above; the PHP curl client trusts phantom's MITM
    // cert via the injected -d curl.cainfo=<path>, with normal verification.
    let phantom_output = Command::new(phantom_bin)
        .args([
            "--backend",
            "proxy",
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--insecure",
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .arg("--")
        .arg("php")
        .arg(app_dir.join("client.php"))
        .env("BACKEND_HTTP_URL", format!("http://127.0.0.1:{http_port}"))
        .env(
            "BACKEND_HTTPS_URL",
            format!("https://localhost:{https_port}"),
        )
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();

    assert!(
        phantom_output.status.success(),
        "phantom exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    // ── Parse JSONL traces ───────────────────────────────────────────────
    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert_eq!(
        traces.len(),
        4,
        "Expected 4 traces (2 HTTP + 2 HTTPS), got {}.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len(),
    );

    // ── HTTP: GET /api/health ────────────────────────────────────────────
    let health_http = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/health") && url.starts_with("http://")
        })
        .expect("missing HTTP GET /api/health");
    assert_eq!(health_http["method"], "GET");
    assert_eq!(health_http["status_code"], 200);
    assert_eq!(
        health_http["request_headers"]["x-phantom-client"],
        "php-curl"
    );
    assert!(
        health_http["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok"))
    );

    // ── HTTP: GET /api/users ─────────────────────────────────────────────
    let users_http = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/users") && url.starts_with("http://") && t["method"] == "GET"
        })
        .expect("missing HTTP GET /api/users");
    assert_eq!(users_http["status_code"], 200);
    assert!(
        users_http["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("Alice"))
    );

    // ── HTTPS: GET /api/health ───────────────────────────────────────────
    let health_https = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/health") && url.starts_with("https://")
        })
        .expect("missing HTTPS GET /api/health");
    assert_eq!(health_https["method"], "GET");
    assert_eq!(health_https["status_code"], 200);
    assert!(
        health_https["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok"))
    );

    // ── HTTPS: POST /api/users ───────────────────────────────────────────
    let create_https = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/users") && url.starts_with("https://") && t["method"] == "POST"
        })
        .expect("missing HTTPS POST /api/users");
    assert_eq!(create_https["status_code"], 201);
    assert!(
        create_https["request_body"]
            .as_str()
            .is_some_and(|b| b.contains("Charlie"))
    );
    assert!(
        create_https["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("Charlie"))
    );

    // ── Cross-cutting checks ─────────────────────────────────────────────
    for (i, t) in traces.iter().enumerate() {
        assert!(
            t["trace_id"].as_str().is_some_and(|s| !s.is_empty()),
            "trace[{i}] trace_id"
        );
        assert!(
            t["span_id"].as_str().is_some_and(|s| !s.is_empty()),
            "trace[{i}] span_id"
        );
        assert!(
            t["timestamp_ms"].as_u64().is_some_and(|v| v > 0),
            "trace[{i}] timestamp_ms"
        );
        assert_eq!(
            t["request_headers"]["x-phantom-client"], "php-curl",
            "trace[{i}] x-phantom-client header"
        );
    }

    eprintln!("All 4 PHP curl traces verified (HTTP + HTTPS).");
}
