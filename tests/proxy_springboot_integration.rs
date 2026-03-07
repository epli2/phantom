//! Integration test: phantom proxy transparently traces a Spring Boot app
//!
//! Verifies non-invasive proxy tracing: the Spring Boot CommandLineRunner reads
//! `HTTP_PROXY` (set automatically by phantom) and wires it into
//! `java.net.http.HttpClient`. No application business logic is proxy-aware.
//!
//! Tests both HTTP and HTTPS (MITM) capture.
//!
//! Requirements: `java` (17+) and `mvn` on PATH.
//! Run: `cargo test --test proxy_springboot_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
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
// Mock backends
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
fn test_proxy_captures_springboot_app_traffic() {
    // Pre-flight: require java and mvn
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
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/springboot-app");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    // Build the Spring Boot fat JAR
    // -DskipTests: no unit tests in the app, avoids needing a test runner.
    // --no-transfer-progress: suppress download progress bars in CI logs.
    let mvn_status = Command::new("mvn")
        .args(["package", "-q", "--no-transfer-progress", "-DskipTests", "-f"])
        .arg(app_dir.join("pom.xml"))
        .status()
        .expect("mvn package");
    assert!(mvn_status.success(), "mvn package failed");

    // Locate the fat JAR at the deterministic Maven output path
    let jar_path = app_dir
        .join("target")
        .join("phantom-springboot-client-0.0.1-SNAPSHOT.jar");
    assert!(
        jar_path.exists(),
        "JAR not found at {}: did mvn package succeed?",
        jar_path.display()
    );

    // Allocate ports
    let http_port = available_port();
    let https_port = available_port();
    let proxy_port = available_port();

    // Generate self-signed cert for the HTTPS mock backend
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    // Start HTTP backend
    let _http_thread = start_http_backend(http_port);
    assert!(
        wait_for_port(http_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    // Start HTTPS backend
    let _https_thread = start_https_backend(https_port, cert_der, key_der);
    assert!(
        wait_for_port(https_port, Duration::from_secs(3)),
        "HTTPS backend did not start"
    );

    // Run phantom with `-- java -jar {jar}`
    // phantom sets HTTP_PROXY automatically; the Spring Boot app reads it.
    // --insecure: phantom's proxy skips cert validation for the self-signed backend cert.
    //             The Java client's trust-all SSLContext handles phantom's MITM CA cert.
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

    // Parse JSONL traces
    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // Expect 4 traces: 2 HTTP (GET health, GET users) + 2 HTTPS (GET health, POST users)
    assert_eq!(
        traces.len(),
        4,
        "Expected 4 traces (2 HTTP + 2 HTTPS), got {}.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len(),
    );

    // HTTP: GET /api/health
    let health_http = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/health") && url.starts_with("http://")
        })
        .expect("missing HTTP GET /api/health");
    assert_eq!(health_http["method"], "GET");
    assert_eq!(health_http["status_code"], 200);
    assert!(
        health_http["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok")),
        "HTTP health response_body should contain 'ok'"
    );

    // HTTP: GET /api/users
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
            .is_some_and(|b| b.contains("Alice")),
        "HTTP users response_body should contain 'Alice'"
    );

    // HTTPS: GET /api/health
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
            .is_some_and(|b| b.contains("ok")),
        "HTTPS health response_body should contain 'ok'"
    );

    // HTTPS: POST /api/users
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
            .is_some_and(|b| b.contains("Charlie")),
        "HTTPS create request_body should contain 'Charlie'"
    );
    assert!(
        create_https["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("Charlie")),
        "HTTPS create response_body should contain 'Charlie'"
    );

    // Cross-cutting checks: every trace must have required fields
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

    eprintln!("All 4 traces (2 HTTP + 2 HTTPS) verified.");
}
