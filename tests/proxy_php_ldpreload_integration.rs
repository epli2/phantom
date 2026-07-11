//! Integration test: phantom ldpreload backend traces a PHP curl app (Linux only)
//!
//! Verifies that phantom's LD_PRELOAD backend (`crates/phantom-agent`) captures
//! HTTP and HTTPS traffic from PHP's curl extension with ZERO PHP-specific Rust
//! code: the agent hooks libc `send()`/`recv()` (plain-text HTTP) and OpenSSL
//! `SSL_write()`/`SSL_read()` (HTTPS, above the TLS layer), which works for any
//! process dynamically linked against the system libssl — including PHP's curl
//! extension. This is unlike the `proxy` backend, which needed PHP-specific
//! injection (see `tests/proxy_php_integration.rs`).
//!
//! Since ldpreload connects the client directly to the real backend server (no
//! MITM), there is no phantom CA to trust here. `client.php` is told to accept
//! the mock HTTPS backend's self-signed certificate via `PHANTOM_TEST_INSECURE`
//! — a test-fixture concern, unrelated to phantom's transparent injection (which
//! does not touch the PHP process at all under this backend).
//!
//! Requirements: `php` (with the curl extension) on PATH, Linux only.
//! Run: `cargo build -p phantom-agent && cargo test --test proxy_php_ldpreload_integration`

#![cfg(target_os = "linux")]

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (mirrors proxy_node_integration.rs / proxy_php_integration.rs)
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

/// Builds `phantom-agent` if the dylib isn't already present, then returns its
/// path. Mirrors how `proxy_java_clients_integration.rs` builds the fat JAR
/// on demand rather than requiring a separate pre-build step.
fn ensure_agent_lib() -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib_path = workspace_root.join("target/debug/libphantom_agent.so");
    if !lib_path.exists() {
        let status = Command::new("cargo")
            .args(["build", "-p", "phantom-agent"])
            .current_dir(workspace_root)
            .status()
            .expect("run cargo build -p phantom-agent");
        assert!(status.success(), "cargo build -p phantom-agent failed");
    }
    assert!(lib_path.exists(), "agent lib not found at {lib_path:?}");
    lib_path
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
// Mock backend — HTTPS (rustls, self-signed, unrelated to any phantom CA)
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
fn test_ldpreload_captures_php_curl_traffic() {
    // Pre-flight: php (with curl extension) available?
    let php_check = Command::new("php")
        .args(["-m"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match &php_check {
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

    let agent_lib = ensure_agent_lib();
    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/php-app");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let http_port = available_port();
    let https_port = available_port();

    // ── Generate self-signed cert for the mock HTTPS backend ──────────────
    // There is no phantom MITM CA under ldpreload (it connects the client
    // directly to the real backend), so client.php is told (via
    // PHANTOM_TEST_INSECURE) to skip peer verification for this cert.
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    // ── Start mock backends ────────────────────────────────────────────────
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

    // ── Run phantom with `--backend ldpreload -- php client.php` ──────────
    // No proxy config or CA is injected for ldpreload; client.php talks
    // directly to the backends and the agent captures traffic at the libc /
    // OpenSSL layer underneath it.
    let phantom_output = Command::new(phantom_bin)
        .args(["--backend", "ldpreload", "--output", "jsonl", "--agent-lib"])
        .arg(&agent_lib)
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .arg("--")
        .arg("php")
        .arg(app_dir.join("client.php"))
        .env("BACKEND_HTTP_URL", format!("http://127.0.0.1:{http_port}"))
        .env(
            "BACKEND_HTTPS_URL",
            format!("https://localhost:{https_port}"),
        )
        .env("PHANTOM_TEST_INSECURE", "1")
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();

    assert!(
        phantom_output.status.success(),
        "phantom exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    // ── Parse JSONL traces ─────────────────────────────────────────────────
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

    // ── HTTP: GET /api/health ──────────────────────────────────────────────
    let health_http = traces
        .iter()
        .find(|t| {
            t["method"] == "GET" && t["url"].as_str().is_some_and(|u| u.contains("/api/health"))
        })
        .expect("missing HTTP GET /api/health");
    assert_eq!(health_http["status_code"], 200);
    assert!(
        health_http["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok"))
    );

    // ── HTTP: GET /api/users ───────────────────────────────────────────────
    let users_trace = traces
        .iter()
        .find(|t| {
            t["method"] == "GET" && t["url"].as_str().is_some_and(|u| u.contains("/api/users"))
        })
        .expect("missing GET /api/users");
    assert_eq!(users_trace["status_code"], 200);
    assert!(
        users_trace["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("Alice"))
    );

    // ── POST /api/users (created via one of the two backends) ─────────────
    let create_trace = traces
        .iter()
        .find(|t| t["method"] == "POST")
        .expect("missing POST /api/users");
    assert_eq!(create_trace["status_code"], 201);
    assert!(
        create_trace["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("Charlie"))
    );

    // ── Cross-cutting checks ───────────────────────────────────────────────
    for (i, t) in traces.iter().enumerate() {
        assert!(
            t["trace_id"].as_str().is_some_and(|s| !s.is_empty()),
            "trace[{i}] trace_id"
        );
        assert_eq!(
            t["request_headers"]["x-phantom-client"], "php-curl",
            "trace[{i}] x-phantom-client header"
        );
    }

    eprintln!(
        "All 4 PHP curl traces verified via ldpreload backend (HTTP + HTTPS, zero PHP-specific code)."
    );
}
