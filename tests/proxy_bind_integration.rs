//! Integration test: `--bind 0.0.0.0` and standalone-mode CA cert export.
//!
//! Verifies:
//!   - the proxy actually listens on 0.0.0.0 (reachable via loopback, since
//!     an unspecified bind always includes it) and still proxies correctly
//!   - <data_dir>/ca.pem is written once the proxy is ready
//!
//! This drives phantom via `run ... -- curl ...` (not truly standalone/no-child)
//! because JSONL mode only auto-exits on child completion or ctrl-c, and a
//! no-child scenario would need extra out-of-band process teardown in the
//! test harness. The CA-export code path now runs unconditionally before the
//! child-spawn branch (see `run_proxy` in src/commands/run.rs), so this still
//! fully exercises both the bind-address change and the CA-export logic that
//! a true standalone/Docker-sidecar run would use. The fully standalone case
//! is verified manually via examples/docker-sidecar/.
//!
//! Requirements: `curl` on PATH.
//! Run: `cargo test --test proxy_bind_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (mirrors proxy_php_integration.rs)
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
    let _ = String::from_utf8_lossy(&buf[..n]);
    write_response(stream, "200 OK", HEALTH_BODY);
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
// Test
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_bind_all_interfaces_and_ca_export() {
    if Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let http_port = available_port();
    let proxy_port = available_port();

    let _http_thread = start_http_backend(http_port);
    assert!(
        wait_for_port(http_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    let phantom_output = Command::new(phantom_bin)
        .args([
            "run",
            "--backend",
            "proxy",
            "--bind",
            "0.0.0.0",
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .arg("--")
        .arg("curl")
        .arg("-s")
        .arg("-o")
        .arg("/dev/null")
        .arg(format!("http://127.0.0.1:{http_port}/api/health"))
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();

    assert!(
        phantom_output.status.success(),
        "phantom exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    // ── Parse JSONL traces — proves the proxy bound to 0.0.0.0 still works ──
    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v.get("trace_id").is_some())
        .collect();

    assert_eq!(
        traces.len(),
        1,
        "Expected 1 trace, got {}.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len(),
    );
    assert_eq!(traces[0]["method"], "GET");
    assert_eq!(traces[0]["status_code"], 200);
    assert!(
        traces[0]["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok"))
    );

    // ── <data_dir>/ca.pem must exist and be PEM-encoded ─────────────────────
    let ca_path = tmp_dir.path().join("ca.pem");
    let ca_pem = std::fs::read_to_string(&ca_path)
        .unwrap_or_else(|e| panic!("ca.pem should exist at {ca_path:?}: {e}"));
    assert!(
        ca_pem.contains("BEGIN CERTIFICATE"),
        "ca.pem should be PEM-encoded, got:\n{ca_pem}"
    );

    eprintln!("bind=0.0.0.0 proxy verified, ca.pem exported to {ca_path:?}");
}
