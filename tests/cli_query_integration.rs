//! CLI integration tests for agent-facing behavior:
//!   - `phantom run` propagates the traced child's exit code
//!   - stdout stays pure JSONL (diagnostics go to stderr)
//!   - `phantom list` / `get` / `stats` query previously captured traces
//!   - query subcommands fail with a lock hint while another process holds
//!     the store
//!
//! Requirements: `curl` on PATH for the capture-based tests (skipped otherwise).
//! Run: `cargo test --test cli_query_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (same patterns as fault_injection.rs)
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

const HEALTH_BODY: &str = r#"{"status":"ok","padding":"0123456789"}"#;

fn handle_stream(stream: &mut (impl Read + IoWrite)) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let first = req.lines().next().unwrap_or("");
    let (status, body) = if first.contains("/api/health") {
        ("200 OK", HEALTH_BODY)
    } else {
        ("404 Not Found", r#"{"error":"Not Found"}"#)
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
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

fn curl_missing() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

/// Run `phantom run -o jsonl <extra_run_flags>` tracing one curl GET against
/// the mock backend, persisting traces into `data_dir`.
fn capture_health_request(data_dir: &Path, extra_run_flags: &[&str]) -> std::process::Output {
    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend = start_http_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "HTTP backend did not start"
    );

    let out = Command::new(phantom_bin)
        .args(["run", "--output", "jsonl", "--port"])
        .arg(proxy_port.to_string())
        .arg("--data-dir")
        .arg(data_dir)
        .args(extra_run_flags)
        .env("no_proxy", "")
        .env("NO_PROXY", "")
        .arg("--")
        .args(["curl", "--silent", "--output", "/dev/null", "--proxy"])
        .arg(format!("http://127.0.0.1:{proxy_port}"))
        .arg(format!("http://127.0.0.1:{backend_port}/api/health"))
        .output()
        .expect("run phantom");
    assert!(
        out.status.success(),
        "phantom run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Run `phantom run -o jsonl` tracing one curl GET against the mock backend,
/// persisting traces into `data_dir`. Returns phantom's stdout.
fn capture_one_health_request(data_dir: &Path) -> String {
    let out = capture_health_request(data_dir, &[]);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn phantom_query(data_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_phantom"))
        .args(args)
        .arg("--data-dir")
        .arg(data_dir)
        .output()
        .expect("run phantom query")
}

// ─────────────────────────────────────────────────────────────────────────────
// Exit code propagation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_run_propagates_child_exit_code() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_phantom"))
        .args(["run", "--output", "jsonl", "--port"])
        .arg(available_port().to_string())
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .args(["--", "sh", "-c", "exit 3"])
        .output()
        .expect("run phantom");
    assert_eq!(
        out.status.code(),
        Some(3),
        "child exit code not propagated; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The end-of-run summary is machine-readable JSON on stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let summary = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["event"] == "exit")
        .expect("exit summary on stderr");
    assert_eq!(summary["child_exit_code"], 3);
}

#[test]
fn test_run_success_exit_code_zero() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_phantom"))
        .args(["run", "--output", "jsonl", "--port"])
        .arg(available_port().to_string())
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .args(["--", "sh", "-c", "exit 0"])
        .output()
        .expect("run phantom");
    assert_eq!(out.status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn test_run_signal_death_maps_to_128_plus_signal() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_phantom"))
        .args(["run", "--output", "jsonl", "--port"])
        .arg(available_port().to_string())
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .args(["--", "sh", "-c", "kill -9 $$"])
        .output()
        .expect("run phantom");
    assert_eq!(
        out.status.code(),
        Some(128 + 9),
        "SIGKILLed child should map to exit 137; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_run_quiet_suppresses_status_and_summary() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_phantom"))
        .args(["--quiet", "run", "--output", "jsonl", "--port"])
        .arg(available_port().to_string())
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .args(["--", "sh", "-c", "exit 5"])
        .output()
        .expect("run phantom");
    // Exit code still propagates in quiet mode.
    assert_eq!(out.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("phantom:"),
        "status lines must be suppressed: {stderr}"
    );
    assert!(
        !stderr.contains("\"event\""),
        "exit summary must be suppressed: {stderr}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// JSONL purity + query subcommands
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_capture_then_query_via_cli() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let stdout = capture_one_health_request(tmp_dir.path());

    // stdout must be pure JSONL: every non-empty line parses as JSON.
    let mut traces = Vec::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON stdout line {line:?}: {e}"));
        traces.push(v);
    }
    assert_eq!(traces.len(), 1, "expected exactly one captured trace");
    assert_eq!(traces[0]["status_code"], 200);
    let span_id = traces[0]["span_id"].as_str().unwrap().to_string();

    // list: status filter matches
    let out = phantom_query(tmp_dir.path(), &["list", "--status", "2xx"]);
    assert!(out.status.success());
    let listed: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["span_id"], span_id.as_str());
    assert!(
        listed[0]["url"].as_str().unwrap().contains("/api/health"),
        "unexpected url: {}",
        listed[0]["url"]
    );

    // list: non-matching filter returns nothing
    let out = phantom_query(tmp_dir.path(), &["list", "--status", "5xx"]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());

    // list: --max-body truncation is flagged and reports the original size
    let out = phantom_query(
        tmp_dir.path(),
        &["list", "--max-body", "4", "--format", "jsonl"],
    );
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["response_body"].as_str().unwrap().len(), 4);
    assert_eq!(v["response_body_truncated"], true);
    assert_eq!(
        v["response_body_bytes"].as_u64().unwrap(),
        HEALTH_BODY.len() as u64
    );

    // list: --headers-only omits bodies but keeps sizes
    let out = phantom_query(tmp_dir.path(), &["list", "--headers-only"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert!(v.get("response_body").is_none());
    assert!(v.get("response_body_bytes").is_some());

    // get: found → exit 0, pretty JSON object
    let out = phantom_query(tmp_dir.path(), &["get", &span_id]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["span_id"], span_id.as_str());

    // get: unknown span → exit 1
    let out = phantom_query(tmp_dir.path(), &["get", "ffffffffffffffff"]);
    assert_eq!(out.status.code(), Some(1));

    // search: positional pattern filters by URL substring
    let out = phantom_query(tmp_dir.path(), &["search", "/api/health"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    let out = phantom_query(tmp_dir.path(), &["search", "/nowhere"]);
    assert!(out.stdout.is_empty());

    // stats: reports the trace count as JSON
    let out = phantom_query(tmp_dir.path(), &["stats"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["total_traces"].is_u64());

    // clear: refuses without --yes, works with it
    let out = phantom_query(tmp_dir.path(), &["clear"]);
    assert_eq!(out.status.code(), Some(1));
    let out = phantom_query(tmp_dir.path(), &["clear", "--yes"]);
    assert!(out.status.success());
    let out = phantom_query(tmp_dir.path(), &["list"]);
    assert!(out.stdout.is_empty());
}

#[test]
fn test_run_stream_respects_max_body_and_reports_summary() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = capture_health_request(tmp_dir.path(), &["--max-body", "4"]);

    // Live JSONL stream honours --max-body and flags the truncation.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().next().expect("one trace"))
        .expect("trace line is JSON");
    assert_eq!(v["response_body"].as_str().unwrap().len(), 4);
    assert_eq!(v["response_body_truncated"], true);
    assert_eq!(
        v["response_body_bytes"].as_u64().unwrap(),
        HEALTH_BODY.len() as u64
    );

    // The stderr exit summary counts the captured traces.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let summary = stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["event"] == "exit")
        .expect("exit summary on stderr");
    assert_eq!(summary["traces_captured"], 1);
    assert_eq!(summary["child_exit_code"], 0);
}

#[test]
fn test_run_stream_headers_only() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let out = capture_health_request(tmp_dir.path(), &["--headers-only"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("one trace")).unwrap();
    assert!(v.get("response_body").is_none());
    assert_eq!(
        v["response_body_bytes"].as_u64().unwrap(),
        HEALTH_BODY.len() as u64
    );
    // The full body is still persisted — only the stream output is reduced.
    let listed = phantom_query(tmp_dir.path(), &["list"]);
    let stored: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["response_body"].as_str().unwrap(), HEALTH_BODY);
}

#[test]
fn test_list_filters_method_time_and_trace_id() {
    if curl_missing() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let stdout = capture_one_health_request(tmp_dir.path());
    let captured: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("one trace")).unwrap();
    let trace_id = captured["trace_id"].as_str().unwrap();

    // --method: GET matches, POST does not.
    let out = phantom_query(tmp_dir.path(), &["list", "--method", "GET"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    let out = phantom_query(tmp_dir.path(), &["list", "--method", "POST"]);
    assert!(out.stdout.is_empty());

    // --since/--until with relative times: the trace is inside (1h ago, now]
    // and outside windows entirely in the past.
    let out = phantom_query(tmp_dir.path(), &["list", "--since", "1h"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    let out = phantom_query(tmp_dir.path(), &["list", "--until", "1h"]);
    assert!(out.stdout.is_empty());

    // --trace-id round trip.
    let out = phantom_query(tmp_dir.path(), &["list", "--trace-id", trace_id]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    let other_id = "0".repeat(32);
    let out = phantom_query(tmp_dir.path(), &["list", "--trace-id", &other_id]);
    assert!(out.stdout.is_empty());

    // --format json produces a pretty-printed array.
    let out = phantom_query(tmp_dir.path(), &["list", "--format", "json"]);
    let arr: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json array");
    assert_eq!(arr.as_array().unwrap().len(), 1);

    // --redact-header masks the named header in output.
    let out = phantom_query(tmp_dir.path(), &["list", "--redact-header", "User-Agent"]);
    let v: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).lines().next().unwrap()).unwrap();
    assert_eq!(v["request_headers"]["user-agent"], "[redacted]");
}

#[test]
fn test_query_invalid_arguments_fail_fast() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    // clap rejects an unparsable status range before touching the store.
    let out = phantom_query(tmp_dir.path(), &["list", "--status", "bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid status range"),
        "stderr should explain the status parse failure"
    );

    // Non-hex span/trace IDs are rejected with a clear message and exit 1.
    let out = phantom_query(tmp_dir.path(), &["get", "not-a-span-id"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid span ID"));

    let out = phantom_query(tmp_dir.path(), &["list", "--trace-id", "xyz"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid trace ID"));

    // Unparsable --since values are rejected.
    let out = phantom_query(tmp_dir.path(), &["list", "--since", "yesterday"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid time"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Store lock UX
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_query_while_store_locked() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    // Hold the store open in this process (simulates a running phantom run/mcp).
    let _store = phantom_storage::FjallTraceStore::open(tmp_dir.path()).expect("open store");

    let out = phantom_query(tmp_dir.path(), &["list"]);
    assert!(
        !out.status.success(),
        "list should fail while the store is locked"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("store lock"),
        "expected lock hint in stderr, got: {stderr}"
    );
}
