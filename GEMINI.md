# Phantom Project Context

Phantom is a next-generation API observability and automatic workflow generation tool written in Rust. It captures network traffic (HTTP/HTTPS) using multiple backends and provides both a terminal-based interface (TUI) and a JSON Lines stream to explore, analyze, and store API traces.

## 🚀 Quick Start

### Building and Running
- **Build the project:** `cargo build`
- **Run with Proxy (default):** `phantom -- <COMMAND>` (e.g., `phantom -- node app.js`)
- **Run with LD_PRELOAD (Linux only):**
  `cargo run -- --backend ldpreload --agent-lib ./target/debug/libphantom_agent.so -- curl http://example.com`
- **Run in JSONL mode:** `phantom --output jsonl -- <COMMAND>`
- **Run tests:** `cargo test`

### Common Examples
- **Trace a Node.js app (HTTP + HTTPS captured automatically):**
  `phantom -- node app.js`
- **Stream traces to jq for filtering:**
  `phantom --output jsonl -- node app.js | jq 'select(.status_code >= 400)'`
- **Capture plain HTTP for any command:**
  `phantom -- curl http://api.example.com/v1/users`

## 🛠 CLI Options
- `-b, --backend <BACKEND>`: Capture backend to use (`proxy` or `ldpreload`). Default: `proxy`.
- `-o, --output <MODE>`: Output mode (`tui` or `jsonl`). Default: `tui`.
- `-p, --port <PORT>`: Port for the proxy backend. Default: `8080`.
- `--insecure`: Disable TLS certificate verification for backend connections.
- `-d, --data-dir <DIR>`: Directory for trace storage. Default: `~/.local/share/phantom/data`.
- `--agent-lib <PATH>`: Path to `libphantom_agent.so` (required for `ldpreload`).
- `-- <COMMAND>`: The command to run with interception injected.

### JSONL Output Schema
When using `--output jsonl`, each line is a JSON object with:
- `trace_id`: W3C-compatible 128-bit trace ID (hex).
- `span_id`: 64-bit span ID (hex).
- `timestamp_ms`: Unix epoch milliseconds.
- `duration_ms`: Round-trip latency in ms.
- `method`: HTTP verb (GET, POST, etc.).
- `url`: Full request URL.
- `status_code`: HTTP response status.
- `protocol_version`: e.g., "HTTP/1.1".
- `request_headers` / `response_headers`: Header maps.
- `request_body` / `response_body`: UTF-8 decoded bodies (optional).

## 🏗 Architecture & Tech Stack

The project is organized as a Rust workspace:

- **`phantom-core`**: Defines core traits (`TraceStore`, `CaptureBackend`) and `HttpTrace`.
- **`phantom-storage`**: Implements `TraceStore` using **Fjall** (LSM-tree).
- **`phantom-capture`**: Implements `CaptureBackend`.
    - **ProxyBackend**: MITM HTTPS interception using `hudsucker`.
    - **Node.js Integration**: Automatically injects `proxy-preload.js` via `--require` to capture HTTPS without code changes.
    - **LdPreloadBackend**: Receives traces from `phantom-agent` via Unix Domain Sockets.
- **`phantom-agent`**: Linux-only `LD_PRELOAD` library hooking `libc` `send`/`recv`.
- **`phantom-tui`**: Interactive UI using **Ratatui**.

### Key Technologies
- **Async Runtime:** `tokio`
- **Storage:** `fjall` (LSM-tree with key-value separation).
- **TUI:** `ratatui`
- **Proxy/MITM:** `hudsucker`
- **Serialization:** `serde`, `serde_json`

## 🛠 Development Conventions

### Coding Style
- Use `anyhow` for applications, `thiserror` for libraries.
- Prefer `Arc<dyn Trait>` for component sharing.
- Follow standard Rust idioms and `clippy`.

### Testing
- `phantom-storage` uses `tempfile` for disk-based tests.
- **Integration Tests:** `tests/proxy_node_integration.rs` verifies the Node.js proxy injection.
- Run all tests: `cargo test`.

### Project Roadmap (from `plan.md`)
- **Userspace eBPF:** Integration with `bpftime` for zero-instrumentation capture (10x faster than uprobes).
- **Workflow Inference:** Automatic generation of **Arazzo Specification** using **LLM** (`candle`) and semantic value correlation.
- **GUI:** Cross-platform desktop interface using **Tauri**.

## 📂 Key Files
- `src/main.rs`: CLI entry point and process spawning logic.
- `crates/phantom-core/src/trace.rs`: `HttpTrace` definition.
- `crates/phantom-storage/src/fjall_store.rs`: Primary storage implementation.
- `crates/phantom-capture/src/proxy.rs`: Proxy-based interception logic.
- `crates/phantom-agent/src/lib.rs`: The `LD_PRELOAD` injection agent.
- `tests/proxy_node_integration.rs`: Node.js integration test suite.
- `plan.md`: Comprehensive technical design (Japanese).
