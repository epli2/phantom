//! Integration test: phantom transparently decodes gzip-compressed response
//! bodies for storage/JSONL while forwarding the wire bytes to the client
//! completely unmodified (still compressed, never truncated).
//!
//! Requirements: `curl` on PATH.
//! Run: `cargo test --test proxy_gzip_integration`

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

fn gzip_encode(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// Start an HTTP backend that always answers with a gzip-encoded JSON body.
fn start_gzip_backend(port: u16, body: Vec<u8>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    })
}

#[test]
fn test_gzip_response_is_decoded_for_storage_but_wire_is_unmodified() {
    if !curl_available() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let curl_out_dir = tempfile::tempdir().expect("tempdir for curl output");
    let curl_out_path = curl_out_dir.path().join("response.bin");

    let plain_json = br#"{"message":"hello, this is gzip compressed"}"#.to_vec();
    let compressed = gzip_encode(&plain_json);

    let backend_port = available_port();
    let proxy_port = available_port();

    let _backend_thread = start_gzip_backend(backend_port, compressed.clone());
    assert!(
        wait_for_port(backend_port, Duration::from_secs(3)),
        "gzip backend did not start"
    );

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
        .arg("-o")
        .arg(&curl_out_path)
        .arg(format!("http://127.0.0.1:{backend_port}/api/gzip"))
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();
    assert!(
        phantom_output.status.success(),
        "phantom/curl exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    // ── Wire bytes must be byte-for-byte identical to what the backend sent ──
    let received = std::fs::read(&curl_out_path).expect("read curl output");
    assert_eq!(
        received, compressed,
        "client must receive the original compressed bytes, unmodified"
    );

    // ── The recorded/JSONL copy must be transparently decoded ──────────────
    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let trace = traces
        .iter()
        .find(|t| t["url"].as_str().is_some_and(|u| u.contains("/api/gzip")))
        .unwrap_or_else(|| {
            panic!("missing trace.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}")
        });

    assert_eq!(trace["status_code"], 200);
    assert_eq!(
        trace["response_content_encoding"].as_str(),
        Some("gzip"),
        "response_content_encoding should record the original encoding"
    );
    assert_eq!(
        trace["response_body"].as_str(),
        Some(std::str::from_utf8(&plain_json).unwrap()),
        "response_body should be the decoded plaintext JSON"
    );
    assert_eq!(trace["response_body_encoding"].as_str(), Some("utf-8"));

    // ── docs/jsonl-schema.md field reference: every schema_version 2 field
    // must be present on every record (optional fields may be `null`/absent
    // only per that document's rules; the required ones below never are).
    assert_eq!(trace["schema_version"], 2, "schema_version must be 2");
    for field in [
        "schema_version",
        "trace_id",
        "span_id",
        "timestamp_ms",
        "duration_ms",
        "method",
        "url",
        "status_code",
        "protocol_version",
        "request_headers",
        "response_headers",
        "request_body_truncated",
        "response_body_truncated",
    ] {
        assert!(
            !trace[field].is_null(),
            "required JSONL field {field:?} missing from record: {trace}"
        );
    }
}
