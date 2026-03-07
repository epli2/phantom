//! Integration test: phantom proxy transparently traces Java HTTP client libraries
//!
//! Verifies that phantom's proxy backend captures HTTP and HTTPS traffic from
//! four major Java HTTP client libraries:
//!
//!   1. JDK java.net.http.HttpClient  (built-in, Java 11+)
//!   2. AsyncHttpClient               (Netty-based)
//!   3. Jetty HttpClient
//!   4. Apache HttpClient 5
//!
//! Each client adds an `x-phantom-client` request header so traces can be
//! identified in the JSONL output.  The pattern mirrors the Node.js
//! `test_proxy_captures_alternative_http_clients` test.
//!
//! Requirements: `java` (17+) and `mvn` on PATH.
//! Run: `cargo test --test proxy_java_clients_integration`

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

fn start_http_backend(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        listener.set_nonblocking(false).expect("set_nonblocking(false)");
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
// Helper
// ─────────────────────────────────────────────────────────────────────────────

fn client_of(t: &serde_json::Value) -> &str {
    t["request_headers"]["x-phantom-client"]
        .as_str()
        .unwrap_or("unknown")
}

// ─────────────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_proxy_captures_java_http_clients() {
    // ── Pre-flight: require java and mvn ──────────────────────────────────
    if Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("SKIP: `java` not found");
        return;
    }
    if Command::new("mvn")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("SKIP: `mvn` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/java-http-clients");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    // ── Build the fat JAR ─────────────────────────────────────────────────
    let mvn_status = Command::new("mvn")
        .args(["package", "-q", "--no-transfer-progress", "-f"])
        .arg(app_dir.join("pom.xml"))
        .status()
        .expect("mvn package");
    assert!(mvn_status.success(), "mvn package failed");

    let jar_path = app_dir.join("target/java-http-clients-0.0.1-SNAPSHOT.jar");
    assert!(jar_path.exists(), "JAR not found at {jar_path:?}");

    let http_port = available_port();
    let https_port = available_port();
    let proxy_port = available_port();

    // ── Generate self-signed cert ─────────────────────────────────────────
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    // ── Start mock backends ───────────────────────────────────────────────
    let _http_thread = start_http_backend(http_port);
    assert!(
        wait_for_port(http_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    let _https_thread = start_https_backend(https_port, cert_der, key_der);
    assert!(
        wait_for_port(https_port, Duration::from_secs(3)),
        "HTTPS backend did not start"
    );

    // ── Run phantom with `-- java -jar client.jar` ────────────────────────
    // phantom sets HTTP_PROXY automatically; the Java app reads it to
    // configure each client's proxy selector.
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
        .arg("java")
        .arg("-jar")
        .arg(&jar_path)
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

    // ── Parse JSONL traces ────────────────────────────────────────────────
    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // 4 clients × 2 schemes (HTTP + HTTPS) = 8 traces
    assert_eq!(
        traces.len(),
        8,
        "Expected 8 traces (4 clients × 2 schemes), got {}.\
         \n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len(),
    );

    // ── Per-client assertions ─────────────────────────────────────────────
    let clients = [
        "jdk-httpclient",
        "async-http-client",
        "jetty-httpclient",
        "apache-httpclient",
    ];

    for client in clients {
        for scheme in ["http", "https"] {
            let t = traces
                .iter()
                .find(|t| {
                    client_of(t) == client
                        && t["url"]
                            .as_str()
                            .is_some_and(|u| u.starts_with(&format!("{scheme}://")))
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing trace for client={client} scheme={scheme}\
                         \n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
                    )
                });

            assert_eq!(t["method"], "GET", "{client} {scheme} method");
            assert_eq!(t["status_code"], 200, "{client} {scheme} status_code");
            assert!(
                t["url"]
                    .as_str()
                    .is_some_and(|u| u.contains("/api/health")),
                "{client} {scheme} url should contain /api/health"
            );
            assert!(
                t["response_body"]
                    .as_str()
                    .is_some_and(|b| b.contains("ok")),
                "{client} {scheme} response_body should contain 'ok'"
            );
        }
    }

    // ── Cross-cutting checks ──────────────────────────────────────────────
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
        assert!(
            t["request_headers"].is_object(),
            "trace[{i}] request_headers"
        );
        assert!(
            t["response_headers"].is_object(),
            "trace[{i}] response_headers"
        );
    }

    eprintln!(
        "All 8 traces verified. clients: {:?}",
        traces.iter().map(client_of).collect::<Vec<_>>()
    );
}
