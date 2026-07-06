//! Integration test: phantom transparently traces a Python client using only
//! the standard library (urllib.request) — HTTP_PROXY/HTTPS_PROXY routing
//! and SSL_CERT_FILE-based CA trust, both auto-injected by phantom, with
//! zero proxy-aware code in the client itself.
//!
//! Requirements: `python3` on PATH.
//! Run: `cargo test --test proxy_python_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
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

const HEALTH_BODY: &str = r#"{"status":"ok"}"#;

fn write_response(stream: &mut impl IoWrite) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{HEALTH_BODY}",
        HEALTH_BODY.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn handle_stream(stream: &mut (impl Read + IoWrite)) {
    let mut buf = [0u8; 8192];
    match stream.read(&mut buf) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    write_response(stream);
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

#[test]
fn test_proxy_captures_python_stdlib_client() {
    if Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("SKIP: `python3` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/python-app");
    let tmp_dir = tempfile::tempdir().expect("tempdir");

    let http_port = available_port();
    let https_port = available_port();
    let proxy_port = available_port();

    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("generate cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    let _http_thread = start_http_backend(http_port);
    assert!(
        wait_for_port(http_port, Duration::from_secs(3)),
        "HTTP backend"
    );
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
        ])
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .arg("--")
        .arg("python3")
        .arg(app_dir.join("client.py"))
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
        "phantom/python3 exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert_eq!(
        traces.len(),
        2,
        "expected 1 HTTP + 1 HTTPS trace, got {}.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len()
    );

    for scheme in ["http", "https"] {
        let t = traces
            .iter()
            .find(|t| {
                t["url"]
                    .as_str()
                    .is_some_and(|u| u.starts_with(&format!("{scheme}://")))
            })
            .unwrap_or_else(|| panic!("missing {scheme}:// trace.\n  stderr:\n{stderr_buf}"));
        assert_eq!(t["status_code"], 200, "{scheme} status");
        assert_eq!(
            t["request_headers"]["x-phantom-client"], "python",
            "{scheme} client header"
        );
        assert!(
            t["response_body"]
                .as_str()
                .is_some_and(|b| b.contains("ok")),
            "{scheme} response body"
        );
    }
}
