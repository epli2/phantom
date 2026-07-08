//! Integration test: `--redact` masks sensitive headers and JSON body fields
//! in the JSONL output, end to end through the real proxy.
//!
//! Requirements: `curl` on PATH.
//! Run: `cargo test --test redaction_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn curl_available() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Start an HTTP backend that echoes back a JSON body containing a
/// "password" field, regardless of the request.
fn start_backend(port: u16) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let body = br#"{"username":"alice","password":"hunter2"}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    })
}

#[test]
fn test_redact_masks_header_and_body_field() {
    if !curl_available() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend_thread = start_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "backend did not start"
    );

    let phantom_output = Command::new(phantom_bin)
        .args([
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--redact",
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .arg("--")
        .arg("curl")
        .arg("-sS")
        .arg("-H")
        .arg("Authorization: Bearer supersecret")
        .arg("-o")
        .arg("/dev/null")
        .arg(format!("http://127.0.0.1:{backend_port}/api/login"))
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();
    assert!(
        phantom_output.status.success(),
        "phantom/curl exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let trace = traces
        .iter()
        .find(|t| t["url"].as_str().is_some_and(|u| u.contains("/api/login")))
        .unwrap_or_else(|| {
            panic!("missing trace.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}")
        });

    assert_eq!(
        trace["request_headers"]["authorization"].as_str(),
        Some("[REDACTED]"),
        "authorization header must be redacted"
    );

    let response_body = trace["response_body"]
        .as_str()
        .expect("response_body should be present");
    let response_json: serde_json::Value =
        serde_json::from_str(response_body).expect("response_body should be valid JSON");
    assert_eq!(
        response_json["password"], "[REDACTED]",
        "password field must be redacted in the response body"
    );
    assert_eq!(
        response_json["username"], "alice",
        "unrelated fields must survive redaction untouched"
    );
}

#[test]
fn test_without_redact_flag_values_are_untouched() {
    if !curl_available() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend_thread = start_backend(backend_port);
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "backend did not start"
    );

    // No --redact this time.
    let phantom_output = Command::new(phantom_bin)
        .args([
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .arg("--")
        .arg("curl")
        .arg("-sS")
        .arg("-H")
        .arg("Authorization: Bearer supersecret")
        .arg("-o")
        .arg("/dev/null")
        .arg(format!("http://127.0.0.1:{backend_port}/api/login"))
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    assert!(phantom_output.status.success());

    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let trace = traces
        .iter()
        .find(|t| t["url"].as_str().is_some_and(|u| u.contains("/api/login")))
        .expect("missing trace");

    assert_eq!(
        trace["request_headers"]["authorization"].as_str(),
        Some("Bearer supersecret"),
        "without --redact, header values must be untouched"
    );
}
