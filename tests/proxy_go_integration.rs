//! Integration test: phantom transparently traces a Go client's plain HTTP
//! request using only net/http — HTTP_PROXY routing
//! (http.ProxyFromEnvironment), with zero proxy-aware code in the client.
//!
//! HTTPS is intentionally NOT exercised here — see docs/compatibility.md for
//! two real limitations this test uncovered while being written:
//!
//! 1. Go's `net/http.ProxyFromEnvironment` unconditionally refuses to proxy
//!    requests whose host is `localhost` or any loopback IP (127.0.0.0/8,
//!    ::1), regardless of HTTP_PROXY/HTTPS_PROXY/NO_PROXY. This is intentional
//!    upstream Go behavior, not a phantom bug. The mock backend below
//!    therefore binds to a non-loopback local address (discovered
//!    dynamically, since it varies by host/sandbox) so the request actually
//!    reaches phantom's proxy instead of silently bypassing it.
//! 2. phantom's MITM leaf certificates only ever carry a DNS-name SAN (never
//!    an IP SAN), even when the CONNECT target is a raw IP literal. Go (and
//!    any client doing strict RFC 6125 IP-literal hostname verification)
//!    rejects such a certificate outright — this is a phantom/hudsucker gap,
//!    not specific to Go, and is why this test can't add an HTTPS case
//!    without also changing the backend to use a real DNS name (which would
//!    require a hosts-file entry this test can't portably assume).
//!
//! Requirements: `go` on PATH.
//! Run: `cargo test --test proxy_go_integration`

use std::io::{Read, Write as IoWrite};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Find a local, non-loopback IP address to bind the test backend to. Uses
/// the classic UDP "connect" trick: connecting a UDP socket doesn't send any
/// packets, but makes the OS pick the source address it would use to reach
/// the given (unreachable is fine) target, which is normally the host's real
/// outbound-facing address rather than 127.0.0.1.
fn non_loopback_local_ip() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind udp probe socket");
    socket.connect("8.8.8.8:80").expect("connect udp probe");
    socket.local_addr().expect("local_addr").ip()
}

fn available_port_on(ip: IpAddr) -> u16 {
    TcpListener::bind((ip, 0))
        .expect("bind :0")
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_port(addr: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(addr).is_ok() {
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

fn start_http_backend(addr: SocketAddr) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let listener = TcpListener::bind(addr).unwrap();
        for stream in listener.incoming() {
            match stream {
                Ok(mut s) => handle_stream(&mut s),
                Err(_) => break,
            }
        }
    })
}

#[test]
fn test_proxy_captures_go_net_http_client() {
    if Command::new("go")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("SKIP: `go` not found");
        return;
    }

    let phantom_bin = env!("CARGO_BIN_EXE_phantom");
    let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/apps/go-app");
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    // `go run` needs a writable GOCACHE; point it at a scratch dir so this
    // works in sandboxes without a preconfigured Go build cache.
    let go_cache_dir = tempfile::tempdir().expect("tempdir for GOCACHE");

    // See the module doc comment: Go never proxies loopback destinations, so
    // the backend must live on a real (non-127.0.0.1) local address.
    let backend_ip = non_loopback_local_ip();
    let http_port = available_port_on(backend_ip);
    let proxy_port = available_port_on(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let http_addr = SocketAddr::new(backend_ip, http_port);

    let _http_thread = start_http_backend(http_addr);
    assert!(
        wait_for_port(&http_addr.to_string(), Duration::from_secs(3)),
        "HTTP backend"
    );

    let phantom_output = Command::new(phantom_bin)
        .args(["--output", "jsonl", "--port", &proxy_port.to_string()])
        .arg("--data-dir")
        .arg(tmp_dir.path())
        .arg("--")
        .arg("go")
        .arg("run")
        .arg(app_dir.join("client.go"))
        .env("BACKEND_HTTP_URL", format!("http://{http_addr}"))
        .env("GOCACHE", go_cache_dir.path())
        .output()
        .expect("run phantom");

    let stdout_buf = String::from_utf8_lossy(&phantom_output.stdout).into_owned();
    let stderr_buf = String::from_utf8_lossy(&phantom_output.stderr).into_owned();
    assert!(
        phantom_output.status.success(),
        "phantom/go exited non-zero.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}"
    );

    let traces: Vec<serde_json::Value> = stdout_buf
        .lines()
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert_eq!(
        traces.len(),
        1,
        "expected 1 HTTP trace, got {}.\n  stdout:\n{stdout_buf}\n  stderr:\n{stderr_buf}",
        traces.len()
    );

    let t = &traces[0];
    assert_eq!(t["status_code"], 200, "status");
    assert_eq!(
        t["request_headers"]["x-phantom-client"], "go",
        "client header"
    );
    assert!(
        t["response_body"]
            .as_str()
            .is_some_and(|b| b.contains("ok")),
        "response body"
    );
}
