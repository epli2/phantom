# Phantom Project Context

Phantom is a next-generation API observability and automatic workflow generation tool written in Rust. It captures network traffic (HTTP/HTTPS) using multiple backends and provides both a terminal-based interface (TUI) and a JSON Lines stream to explore, analyze, and store API traces.

## 🚀 Quick Start

### Building and Running
- **Build the project:** `cargo build`
- **Run with Proxy (default):** `cargo run` (Proxy on port 8080)
- **Run with LD_PRELOAD (Linux only):**
  `cargo run -- --backend ldpreload --agent-lib ./target/debug/libphantom_agent.so -- curl http://example.com`
- **Run in JSONL mode:** `cargo run -- --output jsonl`
- **Run tests:** `cargo test`

### CLI Options
- `-b, --backend <BACKEND>`: Capture backend to use (`proxy` or `ldpreload`).
- `-o, --output <MODE>`: Output mode (`tui` or `jsonl`).
- `-p, --port <PORT>`: Port for the proxy backend (default: 8080).
- `-d, --data-dir <DIR>`: Directory for trace storage (default: `~/.local/share/phantom/data`).
- `--agent-lib <PATH>`: Path to `libphantom_agent.so` (required for `ldpreload`).
- `-- <COMMAND>`: The command to run with `LD_PRELOAD` injected.

## 🏗 Architecture & Tech Stack

The project is organized as a Rust workspace with the following crates:

- **`phantom-core`**: Defines core traits (`TraceStore`, `CaptureBackend`), data structures (`HttpTrace`), and error types.
- **`phantom-storage`**: Implements `TraceStore` using **Fjall**, a high-performance LSM-tree storage engine.
- **`phantom-capture`**: Implements `CaptureBackend`.
    - **ProxyBackend**: MITM HTTPS interception using `hudsucker`.
    - **LdPreloadBackend**: Receives traces from the `phantom-agent` via Unix Domain Sockets.
- **`phantom-agent`**: A Linux-only `LD_PRELOAD` shared library that hooks `libc` functions (`send`, `recv`, `close`) to capture plain-text HTTP traffic.
- **`phantom-tui`**: Terminal user interface using **Ratatui**.

### Key Technologies
- **Async Runtime:** `tokio`
- **Storage Engine:** `fjall`
- **TUI Framework:** `ratatui`
- **Proxy/MITM:** `hudsucker`
- **LD_PRELOAD Hooking:** `redhook`
- **Serialization:** `serde`, `serde_json`
- **Trace Context:** W3C Trace Context compatible (128-bit Trace ID, 64-bit Span ID).

## 🛠 Development Conventions

### Coding Style
- Follow standard Rust idioms and `clippy` suggestions.
- Use `anyhow` for application-level error handling and `thiserror` for library-level errors.
- Prefer `Arc<dyn Trait>` for sharing state between components.

### Testing
- `phantom-storage` uses `tempfile` for testing LSM-tree operations.
- Ensure all tests pass before submitting changes: `cargo test`.

### Project Roadmap (from `plan.md`)
- **Userspace eBPF:** Integration with `bpftime` for zero-instrumentation capture without kernel overhead.
- **Workflow Inference:** Automatic generation of **Arazzo Specification** and **OpenAPI Spec** using **LLM** (`candle`) and semantic analysis.
- **GUI:** Potential future integration with `Tauri`.

## 📂 Key Files
- `src/main.rs`: Entry point, CLI parsing, and component wiring.
- `crates/phantom-core/src/trace.rs`: `HttpTrace` definition.
- `crates/phantom-core/src/capture.rs`: `CaptureBackend` trait.
- `crates/phantom-core/src/storage.rs`: `TraceStore` trait.
- `crates/phantom-storage/src/fjall_store.rs`: Primary storage implementation.
- `crates/phantom-capture/src/proxy.rs`: Proxy-based interception logic.
- `crates/phantom-capture/src/ldpreload.rs`: `LD_PRELOAD` backend listener.
- `crates/phantom-agent/src/lib.rs`: The `LD_PRELOAD` injection agent.
- `crates/phantom-tui/src/lib.rs`: TUI entry point.
- `plan.md`: Comprehensive technical design and research document.
