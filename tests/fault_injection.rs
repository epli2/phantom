//! Fault injection integration tests.
//!
//! Verifies that `--fault` rules are correctly applied by the phantom proxy:
//!   - `error:<status>` returns a synthetic HTTP error without forwarding
//!   - `delay:<ms>`     adds measurable latency to the round-trip
//!   - `error:<status>:<url_pattern>` limits injection to matching URLs
//!
//! Requirements: `curl` on PATH (tests are skipped otherwise).
//! Run: `cargo test --test fault_injection`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
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
// Mock HTTP backend
// ─────────────────────────────────────────────────────────────────────────────

const HEALTH_BODY: &str = r#"{"status":"ok"}"#;
const USERS_BODY: &str = r#"[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"}]"#;

fn route_request(req: &str) -> (&str, &str) {
    let first = req.lines().next().unwrap_or("");
    if first.starts_with("GET") && first.contains("/api/health") {
        ("200 OK", HEALTH_BODY)
    } else if first.starts_with("GET") && first.contains("/api/users") {
        ("200 OK", USERS_BODY)
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
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => handle_stream(&mut s),
                Err(_) => break,
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Test utilities
// ─────────────────────────────────────────────────────────────────────────────

fn parse_traces(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Returns `true` if curl is not found on PATH (caller should skip the test).
fn curl_missing() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

/// Build the base curl arguments used in every fault injection test.
///
/// Key flags:
///   --silent         suppress progress meters / error messages
///   --output /dev/null  discard the response body so it doesn't mix into phantom's
///                    JSONL stdout; we verify the response via the trace record.
///   --proxy <addr>   force curl through the phantom proxy even for localhost
///                    targets (curl by default skips the proxy for 127.0.0.1).
fn curl_args(proxy_port: u16) -> Vec<String> {
    vec![
        "--silent".to_string(),
        "--output".to_string(),
        "/dev/null".to_string(),
        "--proxy".to_string(),
        format!("http://127.0.0.1:{proxy_port}"),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 1: error injection — always return a synthetic HTTP 503
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fault_error_always() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend = start_http_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    // Run phantom with --fault error:503 and a curl child.
    // no_proxy / NO_PROXY must be cleared so curl routes 127.0.0.1 through the proxy.
    let out = Command::new(phantom_bin)
        .args([
            "run",
            "--backend",
            "proxy",
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .args(["--fault", "error:503"])
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .arg("--")
        .arg("curl")
        .args(curl_args(proxy_port))
        .arg(format!("http://127.0.0.1:{backend_port}/api/health"))
        .output()
        .expect("run phantom");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        out.status.success(),
        "phantom exited non-zero.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let traces = parse_traces(&stdout);
    assert_eq!(
        traces.len(),
        1,
        "expected 1 trace, got {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        traces.len()
    );

    let t = &traces[0];

    // Status code must be the injected 503.
    assert_eq!(
        t["status_code"].as_u64(),
        Some(503),
        "status_code should be 503 (fault injected), trace: {t}"
    );

    // Response body must contain the fault marker.
    let body = t["response_body"].as_str().unwrap_or("");
    assert!(
        body.contains("fault"),
        "response_body should contain 'fault', got: {body:?}"
    );

    // The x-fault-injected response header must be set.
    let fault_header = t["response_headers"]["x-fault-injected"].as_str();
    assert_eq!(
        fault_header,
        Some("phantom"),
        "x-fault-injected header should be 'phantom', got: {fault_header:?}"
    );

    // Trace and span IDs must be present.
    assert!(
        t["trace_id"].as_str().is_some_and(|s| !s.is_empty()),
        "trace_id missing"
    );
    assert!(
        t["span_id"].as_str().is_some_and(|s| !s.is_empty()),
        "span_id missing"
    );

    eprintln!("test_fault_error_always: OK — status=503, x-fault-injected=phantom");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: delay injection — duration_ms must be >= injected delay
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fault_delay_adds_latency() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend = start_http_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    const DELAY_MS: u64 = 300;

    let out = Command::new(phantom_bin)
        .args([
            "run",
            "--backend",
            "proxy",
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .args(["--fault", &format!("delay:{DELAY_MS}ms")])
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .arg("--")
        .arg("curl")
        .args(curl_args(proxy_port))
        .arg(format!("http://127.0.0.1:{backend_port}/api/health"))
        .output()
        .expect("run phantom");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        out.status.success(),
        "phantom exited non-zero.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let traces = parse_traces(&stdout);
    assert_eq!(
        traces.len(),
        1,
        "expected 1 trace, got {}.\nstdout:\n{stdout}\nstderr:\n{stderr}",
        traces.len()
    );

    let t = &traces[0];

    // The backend returned 200 — the real response was forwarded.
    assert_eq!(
        t["status_code"].as_u64(),
        Some(200),
        "status_code should be 200 (real backend response), trace: {t}"
    );

    // Duration must reflect the injected delay.
    let duration_ms = t["duration_ms"].as_u64().expect("duration_ms present");
    assert!(
        duration_ms >= DELAY_MS,
        "duration_ms ({duration_ms}) should be >= injected delay ({DELAY_MS}ms)"
    );

    eprintln!(
        "test_fault_delay_adds_latency: OK — status=200, duration={duration_ms}ms (>= {DELAY_MS}ms)"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 3: URL pattern filter — only matching URLs get the fault
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_fault_url_pattern_filter() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let backend_port = available_port();

    let _backend = start_http_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    // Rule: only inject 503 for URLs containing "/api/health"
    let fault_spec = "error:503:/api/health";

    // ── Request 1: /api/health — should get 503 (fault injected) ────────
    let tmp1 = tempfile::tempdir().expect("tempdir");
    let proxy_port1 = available_port();

    let out1 = Command::new(phantom_bin)
        .args([
            "run",
            "--backend",
            "proxy",
            "--output",
            "jsonl",
            "--port",
            &proxy_port1.to_string(),
            "--data-dir",
        ])
        .arg(tmp1.path())
        .args(["--fault", fault_spec])
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .arg("--")
        .arg("curl")
        .args(curl_args(proxy_port1))
        .arg(format!("http://127.0.0.1:{backend_port}/api/health"))
        .output()
        .expect("run phantom (health)");

    let stdout1 = String::from_utf8_lossy(&out1.stdout).into_owned();
    let stderr1 = String::from_utf8_lossy(&out1.stderr).into_owned();

    assert!(
        out1.status.success(),
        "phantom exited non-zero (health).\nstdout:\n{stdout1}\nstderr:\n{stderr1}"
    );

    let traces1 = parse_traces(&stdout1);
    assert_eq!(
        traces1.len(),
        1,
        "expected 1 trace for /api/health, got {}.\nstdout:\n{stdout1}",
        traces1.len()
    );
    assert_eq!(
        traces1[0]["status_code"].as_u64(),
        Some(503),
        "/api/health should be 503 (fault injected), trace: {}",
        traces1[0]
    );

    // ── Request 2: /api/users — should get 200 (no fault) ───────────────
    let tmp2 = tempfile::tempdir().expect("tempdir");
    let proxy_port2 = available_port();

    let out2 = Command::new(phantom_bin)
        .args([
            "run",
            "--backend",
            "proxy",
            "--output",
            "jsonl",
            "--port",
            &proxy_port2.to_string(),
            "--data-dir",
        ])
        .arg(tmp2.path())
        .args(["--fault", fault_spec])
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .arg("--")
        .arg("curl")
        .args(curl_args(proxy_port2))
        .arg(format!("http://127.0.0.1:{backend_port}/api/users"))
        .output()
        .expect("run phantom (users)");

    let stdout2 = String::from_utf8_lossy(&out2.stdout).into_owned();
    let stderr2 = String::from_utf8_lossy(&out2.stderr).into_owned();

    assert!(
        out2.status.success(),
        "phantom exited non-zero (users).\nstdout:\n{stdout2}\nstderr:\n{stderr2}"
    );

    let traces2 = parse_traces(&stdout2);
    assert_eq!(
        traces2.len(),
        1,
        "expected 1 trace for /api/users, got {}.\nstdout:\n{stdout2}",
        traces2.len()
    );
    assert_eq!(
        traces2[0]["status_code"].as_u64(),
        Some(200),
        "/api/users should be 200 (fault pattern did not match), trace: {}",
        traces2[0]
    );

    // Verify the real backend response body was forwarded.
    let users_body = traces2[0]["response_body"].as_str().unwrap_or("");
    assert!(
        users_body.contains("Alice"),
        "/api/users response should contain 'Alice', got: {users_body:?}"
    );

    eprintln!(
        "test_fault_url_pattern_filter: OK — /api/health=503 (injected), /api/users=200 (passthrough)"
    );
}
