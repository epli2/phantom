# Phantom Project Context

Phantom is a next-generation API observability and automatic workflow generation tool written in Rust. It captures network traffic (HTTP/HTTPS) and provides a terminal-based interface (TUI) to explore, analyze, and store API traces.

## 🚀 Quick Start

### Building and Running
- **Build the project:** `cargo build`
- **Run the application:** `cargo run` (Defaults to proxy on port 8080)
- **Run with custom port:** `cargo run -- --port 9090`
- **Run tests:** `cargo test` (Runs tests for all workspace members)

### CLI Options
- `-p, --port <PORT>`: Port for the proxy capture backend (default: 8080).
- `-d, --data-dir <DIR>`: Directory for trace storage (default: `~/.local/share/phantom/data`).

## 🏗 Architecture & Tech Stack

The project is organized as a Rust workspace with the following crates:

- **`phantom-core`**: Defines core traits (`TraceStore`, `CaptureBackend`), data structures (`HttpTrace`, `TraceId`, `SpanId`), and error types.
- **`phantom-storage`**: Implements `TraceStore` using **Fjall**, a high-performance LSM-tree storage engine written in pure Rust. It supports key-value separation for efficient storage of large JSON payloads.
- **`phantom-capture`**: Implements `CaptureBackend`. Currently provides a **ProxyCaptureBackend** using `hudsucker` for MITM HTTPS interception.
- **`phantom-tui`**: Implements the terminal user interface using **Ratatui**. It provides a real-time view of captured traces and a way to inspect request/response details.

### Key Technologies
- **Async Runtime:** `tokio`
- **Storage Engine:** `fjall`
- **TUI Framework:** `ratatui`
- **Proxy/MITM:** `hudsucker`
- **Serialization:** `serde`, `serde_json`
- **Trace Context:** W3C Trace Context compatible (128-bit Trace ID, 64-bit Span ID).

## 🛠 Development Conventions

### Coding Style
- Follow standard Rust idioms and `clippy` suggestions.
- Use `anyhow` for application-level error handling and `thiserror` for library-level errors.
- Prefer `Arc<dyn Trait>` for sharing state between the capture backend, storage, and UI.

### Testing
- Each crate contains its own unit and integration tests.
- `phantom-storage` uses `tempfile` for testing LSM-tree operations.
- Ensure all tests pass before submitting changes: `cargo test`.

### Project Roadmap (from `plan.md`)
- **Userspace eBPF:** Integration with `bpftime` for zero-instrumentation capture without kernel overhead.
- **Workflow Inference:** Automatic generation of **Arazzo Specification** and **OpenAPI Spec** from captured traces.
- **Local LLM:** Integration with `candle` for semantic analysis of API dependencies.
- **GUI:** Potential future integration with `Tauri` for rich graphical visualization.

## 📂 Key Files
- `src/main.rs`: Entry point, CLI parsing, and wiring up core components.
- `crates/phantom-core/src/trace.rs`: Definition of the `HttpTrace` structure.
- `crates/phantom-core/src/storage.rs`: `TraceStore` trait definition.
- `crates/phantom-storage/src/fjall_store.rs`: Primary storage implementation.
- `crates/phantom-capture/src/proxy.rs`: Proxy-based traffic interception logic.
- `crates/phantom-tui/src/lib.rs`: TUI entry point and event loop.
- `plan.md`: Comprehensive technical design and research document.
