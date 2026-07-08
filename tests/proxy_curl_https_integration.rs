//! Integration test: proxied curl verifies the MITM'd certificate using the
//! CA trust environment (CURL_CA_BUNDLE / HTTPS_PROXY) that phantom sets
//! automatically — curl needs NO `-k`/`--insecure` flag.
//!
//! Also verifies that the persistent CA is stable across phantom runs
//! (`phantom cert path` + file fingerprint comparison).
//!
//! Requirements: `curl` on PATH.
//! Run: `cargo test --test proxy_curl_https_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers (same pattern as proxy_node_integration.rs)
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

fn curl_available() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

const HEALTH_BODY: &str = r#"{"status":"ok"}"#;

fn handle_stream(stream: &mut (impl Read + IoWrite)) {
    let mut buf = [0u8; 8192];
    let n = match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let _req = String::from_utf8_lossy(&buf[..n]);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{HEALTH_BODY}",
        HEALTH_BODY.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
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
// Test 1: curl verifies the MITM'd cert via the auto-set trust environment
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_curl_https_verifies_phantom_ca_without_insecure_flag() {
    if !curl_available() {
        eprintln!("SKIP: `curl` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let https_port = available_port();
    let proxy_port = available_port();

    // Self-signed cert for the mock backend. phantom's *upstream* connection
    // needs --insecure for this; curl's *client-side* verification of the
    // MITM'd cert must succeed WITHOUT any curl insecure flag — that is what
    // this test proves.
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    let _https_thread = start_https_backend(https_port, cert_der, key_der);
    assert!(
        wait_for_port(https_port, Duration::from_secs(3)),
        "HTTPS backend"
    );

    let phantom_output = Command::new(phantom_bin)
        .args([
            "--output",
            "jsonl",
            "--port",
            &proxy_port.to_string(),
            "--insecure",
            "--data-dir",
        ])
        .arg(tmp_dir.path())
        .arg("--")
        .arg("curl")
        .arg("-sS")
        // Discard curl's own stdout: it shares phantom's inherited stdout,
        // and interleaving curl's response body with phantom's JSONL lines
        // (no synchronization between the two processes) corrupts the JSONL
        // stream. The captured trace is asserted below instead.
        .arg("-o")
        .arg("/dev/null")
        // NOTE: no -k / --insecure here. curl must verify the MITM'd cert
        // via CURL_CA_BUNDLE, and route via HTTPS_PROXY — both set by phantom.
        .arg(format!("https://localhost:{https_port}/api/health"))
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();

    assert!(
        phantom_output.status.success(),
        "phantom/curl exited non-zero (TLS verification failed?).\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let health = traces
        .iter()
        .find(|t| {
            let url = t["url"].as_str().unwrap_or("");
            url.contains("/api/health") && url.starts_with("https://")
        })
        .unwrap_or_else(|| {
            panic!("missing HTTPS trace.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}")
        });
    assert_eq!(health["method"], "GET");
    assert_eq!(health["status_code"], 200);
    assert!(
        health["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok"))
    );

    eprintln!("curl verified the phantom CA without --insecure.");
}

// ─────────────────────────────────────────────────────────────────────────────
// Test 2: the CA is persistent — same certificate across phantom invocations
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_ca_certificate_is_stable_across_runs() {
    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let cert_path_of = || {
        let out = Command::new(phantom_bin)
            .args(["cert", "--data-dir"])
            .arg(tmp_dir.path())
            .arg("path")
            .output()
            .expect("run phantom cert path");
        assert!(
            out.status.success(),
            "phantom cert path failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    let first_path = cert_path_of();
    assert!(
        std::path::Path::new(&first_path).is_file(),
        "cert file exists at {first_path}"
    );
    let first_pem = std::fs::read(&first_path).expect("read cert");

    let second_path = cert_path_of();
    assert_eq!(first_path, second_path, "cert path is stable");
    let second_pem = std::fs::read(&second_path).expect("read cert again");
    assert_eq!(first_pem, second_pem, "cert bytes unchanged across runs");

    // `cert print` streams the same PEM to stdout.
    let print_out = Command::new(phantom_bin)
        .args(["cert", "--data-dir"])
        .arg(tmp_dir.path())
        .arg("print")
        .output()
        .expect("run phantom cert print");
    assert!(print_out.status.success());
    assert_eq!(print_out.stdout, first_pem);
}
