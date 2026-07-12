//! MCP server smoke test: speaks raw newline-delimited JSON-RPC to
//! `phantom mcp` over stdio (no MCP client library involved).
//!
//! Covers: initialize handshake, tools/list, get_stats, start_capture with a
//! traced curl child against an in-process mock backend, capture_status
//! polling, list_traces filtering, get_trace, stop_capture, clean shutdown
//! on stdin EOF.
//!
//! Requirements: `curl` on PATH for the capture round-trip (that part is
//! skipped otherwise). Run: `cargo test --test mcp_stdio_integration`

use std::io::{BufRead, BufReader, Read, Write as IoWrite};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

// ─────────────────────────────────────────────────────────────────────────────
// Mock HTTP backend (same pattern as fault_injection.rs)
// ─────────────────────────────────────────────────────────────────────────────

const HEALTH_BODY: &str = r#"{"status":"ok"}"#;

fn start_http_backend() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock backend");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 8192];
            let n = match s.read(&mut buf) {
                Ok(0) | Err(_) => continue,
                Ok(n) => n,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let (status, body) = if req.lines().next().unwrap_or("").contains("/api/health") {
                ("200 OK", HEALTH_BODY)
            } else {
                ("404 Not Found", r#"{"error":"Not Found"}"#)
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    port
}

fn curl_missing() -> bool {
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
}

// ─────────────────────────────────────────────────────────────────────────────
// Minimal JSON-RPC client over the child's stdio
// ─────────────────────────────────────────────────────────────────────────────

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(data_dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_phantom"))
            .args(["mcp", "--data-dir"])
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phantom mcp");
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            reader,
            next_id: 0,
        };
        client.initialize();
        client
    }

    fn send(&mut self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write to mcp");
        self.stdin.flush().unwrap();
    }

    /// Sends a request and reads lines until its response arrives
    /// (skipping any server-initiated notifications).
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read from mcp");
            assert!(n > 0, "mcp server closed stdout while awaiting {method}");
            let v: Value = serde_json::from_str(&line).expect("mcp response is JSON");
            if v["id"] == json!(id) {
                assert!(
                    v.get("error").is_none(),
                    "{method} returned error: {}",
                    v["error"]
                );
                return v["result"].clone();
            }
        }
    }

    /// Calls an MCP tool and returns the parsed JSON content of its result.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        assert_ne!(
            result["isError"],
            json!(true),
            "tool {name} failed: {result}"
        );
        let content = &result["content"][0];
        // Content is either structured json or a text block containing JSON.
        if let Some(text) = content["text"].as_str() {
            serde_json::from_str(text).unwrap_or_else(|_| json!(text))
        } else {
            content["json"].clone()
        }
    }

    fn initialize(&mut self) {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "phantom-test", "version": "0"}
            }),
        );
        assert!(
            result["capabilities"]["tools"].is_object(),
            "server must advertise tools capability: {result}"
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    /// Closes stdin and expects the server to exit cleanly.
    fn shutdown(mut self) {
        drop(self.stdin);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => {
                    assert!(status.success(), "mcp server exited with {status}");
                    return;
                }
                None if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    panic!("mcp server did not exit within 10s of stdin EOF");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

const EXPECTED_TOOLS: [&str; 7] = [
    "start_capture",
    "stop_capture",
    "capture_status",
    "list_traces",
    "get_trace",
    "get_stats",
    "clear_traces",
];

#[test]
fn test_mcp_stdio_round_trip() {
    let tmp_dir = tempfile::tempdir().expect("tempdir");
    let mut client = McpClient::start(tmp_dir.path());

    // tools/list exposes exactly the expected tool set.
    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in EXPECTED_TOOLS {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }
    assert_eq!(
        names.len(),
        EXPECTED_TOOLS.len(),
        "unexpected tools: {names:?}"
    );

    // Empty store.
    let stats = client.call_tool("get_stats", json!({}));
    assert_eq!(stats["total_traces"], 0);
    assert_eq!(stats["active_sessions"], 0);

    if curl_missing() {
        eprintln!("SKIP: `curl` not found — skipping capture round-trip");
        client.shutdown();
        return;
    }

    // Start a capture that traces one curl request against the mock backend.
    let backend_port = start_http_backend();
    let started = client.call_tool(
        "start_capture",
        json!({
            "command": [
                "sh", "-c",
                format!(
                    "curl --silent --output /dev/null --proxy \"$HTTP_PROXY\" http://127.0.0.1:{backend_port}/api/health"
                )
            ]
        }),
    );
    let session_id = started["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    assert!(started["port"].as_u64().unwrap() > 0);
    assert_eq!(started["state"], "running");

    // Poll until the child exits and the trace is pumped.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = client.call_tool("capture_status", json!({"session_id": session_id}));
        let session = &status["sessions"][0];
        if session["state"] == "exited" && session["trace_count"].as_u64() == Some(1) {
            assert_eq!(session["exit_code"], 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "capture did not finish in time: {status}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // list_traces sees the request; body is present (small) and status matches.
    let listed = client.call_tool("list_traces", json!({"status": "2xx"}));
    let traces = listed["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1, "expected one trace: {listed}");
    assert!(traces[0]["url"].as_str().unwrap().contains("/api/health"));
    let span_id = traces[0]["span_id"].as_str().unwrap().to_string();

    // Non-matching filter.
    let listed = client.call_tool("list_traces", json!({"status": "5xx"}));
    assert!(listed["traces"].as_array().unwrap().is_empty());

    // max_body truncation is flagged.
    let listed = client.call_tool("list_traces", json!({"max_body": 4}));
    let t = &listed["traces"][0];
    assert_eq!(t["response_body"].as_str().unwrap().len(), 4);
    assert_eq!(t["response_body_truncated"], true);

    // get_trace returns full detail.
    let trace = client.call_tool("get_trace", json!({"span_id": span_id}));
    assert_eq!(trace["span_id"], span_id.as_str());
    assert_eq!(trace["response_body"].as_str().unwrap(), HEALTH_BODY);

    // stop_capture tears the session down.
    let stopped = client.call_tool("stop_capture", json!({"session_id": session_id}));
    assert_eq!(stopped["state"], "exited");
    let stats = client.call_tool("get_stats", json!({}));
    assert_eq!(stats["active_sessions"], 0);
    assert_eq!(stats["total_traces"], 1);

    // clear_traces requires confirm and empties the store.
    let cleared = client.call_tool("clear_traces", json!({"confirm": true}));
    assert_eq!(cleared["cleared"], true);
    let listed = client.call_tool("list_traces", json!({}));
    assert!(listed["traces"].as_array().unwrap().is_empty());

    client.shutdown();
}
