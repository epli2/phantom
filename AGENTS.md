# Phantom — Agent Instructions

Phantom is a Rust-based API observability tool that captures HTTP/HTTPS traffic via a MITM proxy (or LD_PRELOAD agent on Linux) and presents it in a terminal UI or streams it as JSON Lines. It is organized as a Cargo workspace of five library crates plus one binary.

---

## Project Layout

```
src/main.rs                  # Binary entry point: CLI parsing, component wiring, child-process spawning
crates/
  phantom-core/              # Domain types, traits, error types — no I/O
  phantom-storage/           # Fjall LSM-tree TraceStore implementation
  phantom-capture/           # Hudsucker MITM proxy + LD_PRELOAD (Linux) CaptureBackend
  phantom-tui/               # Ratatui terminal UI
  phantom-agent/             # LD_PRELOAD dylib (Linux only, hooks libc send/recv)
tests/
  proxy_node_integration.rs  # Integration tests: Node.js proxy capture (HTTP + HTTPS)
  apps/node-app/             # Test Node.js app (client.js, client-alts.js, proxy-preload.js)
  integration/               # Shell-based integration tests
Cargo.toml                   # Workspace root + binary crate
plan.md                      # Japanese-language technical design document
```

**Dependency graph** (no circular dependencies):
```
main → phantom-capture → phantom-core
main → phantom-storage → phantom-core
main → phantom-tui    → phantom-core
phantom-agent          (standalone dylib, no workspace deps)
```

`phantom-core` has zero internal-crate dependencies. All cross-component sharing is done via `Arc<dyn TraitFromPhantomCore>`.

---

## Build, Run, and Check Commands

```sh
cargo build                          # Debug build
cargo build --workspace --all-targets --all-features  # Full build (matches CI)
cargo build --release                # Release build
cargo run                            # Run proxy on default port 8080
cargo run -- --port 9090             # Run with custom port
cargo run -- -- node app.js          # Trace a Node.js app (proxy-preload.js auto-injected)
cargo run -- --output jsonl -- node app.js  # Stream JSONL and exit when child exits
cargo run -- --backend ldpreload --agent-lib ./target/debug/libphantom_agent.so -- curl http://example.com
cargo check                          # Fast type/borrow check (no codegen)
cargo clippy --workspace --all-targets --all-features -- -D warnings  # Lint (CI-exact)
cargo fmt --all                      # Format all code
cargo fmt --all -- --check           # Check formatting (CI-exact)
```

### Makefile Shortcuts

```sh
make build    # cargo build --workspace --all-targets --all-features
make fmt      # cargo fmt --all -- --check
make fmt-fix  # cargo fmt --all
make clippy   # cargo clippy ... -D warnings
make test     # cargo test --workspace --all-targets --all-features
make check    # fmt + clippy + build + test (full CI locally)
```

### CLI Options

| Flag | Default | Description |
|---|---|---|
| `-b, --backend <BACKEND>` | `proxy` | `proxy` (MITM, cross-platform) or `ldpreload` (Linux only) |
| `-o, --output <OUTPUT>` | `tui` | `tui` (interactive) or `jsonl` (stdout stream, auto-exits with child) |
| `-p, --port <PORT>` | `8080` | Proxy capture port |
| `--insecure` | off | Disable TLS verification for backend servers (self-signed certs) |
| `-d, --data-dir <DIR>` | `~/.local/share/phantom/data` | Storage directory |
| `--agent-lib <PATH>` | — | Path to `libphantom_agent.so` (ldpreload backend) |
| `-- <CMD>` | — | Command to spawn and trace automatically |

---

## Node.js Transparent Proxy Injection

When the command after `--` is `node` or `nodejs`, phantom automatically:

1. Writes `proxy-preload.js` (embedded via `include_str!`) to a temp file.
2. Prepends `--require <tempfile>` to the Node arguments.
3. Sets `HTTP_PROXY` / `http_proxy` to `http://127.0.0.1:<PORT>`.
4. Deletes the temp file after the child exits (`TempScript` RAII guard).

`proxy-preload.js` monkey-patches Node's `http`, `https`, and `undici` modules so that **all** outbound requests (including HTTPS) go through the phantom MITM proxy with zero application changes.

### HTTP client support in proxy-preload.js

| Client | Mechanism | HTTP | HTTPS |
|--------|-----------|------|-------|
| `http.request` / `http.get` | Rewritten to absolute URI targeting proxy | ✅ | — |
| `https.request` / `https.get` | Custom `ProxyTunnelAgent` (CONNECT + TLS MITM) | — | ✅ |
| `axios` (npm) | Uses `http`/`https` internally → patched automatically | ✅ | ✅ |
| `undici.request()` | `setGlobalDispatcher(new ProxyAgent(...))` | ✅ | ✅ |
| `globalThis.fetch` HTTP | Patched to route `http://` via `http.request` (bypasses CONNECT) | ✅ | — |
| `globalThis.fetch` HTTPS | Handled by undici ProxyAgent (CONNECT → MITM) | — | ✅ |

**Double-proxy guard**: axios auto-reads `HTTP_PROXY` and formats an absolute-URI request. The `http.request` patch detects this (absolute URI path + target == proxy host:port) and skips re-wrapping.

**fetch HTTP CONNECT issue**: undici's `ProxyAgent` uses `CONNECT` for all `fetch()` requests. Phantom handles `CONNECT` as HTTPS MITM, which breaks plain HTTP. Fix: `proxy-preload.js` patches `globalThis.fetch` to intercept `http://` URLs and route them through `http.request` directly.

---

## JSONL Output Schema

When `--output jsonl` is used, one JSON object is written per line to stdout. All fields are always present unless marked optional.

| Field | Type | Description |
|---|---|---|
| `trace_id` | string | W3C-compatible 128-bit trace ID (hex, 32 chars) |
| `span_id` | string | 64-bit span ID (hex, 16 chars) |
| `timestamp_ms` | number | Unix epoch milliseconds — request start time |
| `duration_ms` | number | Round-trip latency in milliseconds |
| `method` | string | HTTP verb: `"GET"`, `"POST"`, `"PUT"`, `"DELETE"`, … |
| `url` | string | Full request URL (scheme + host + path + query) |
| `status_code` | number | HTTP response status code |
| `protocol_version` | string | HTTP version string, e.g. `"HTTP/1.1"` |
| `request_headers` | object | Lower-cased header names → values |
| `response_headers` | object | Lower-cased header names → values |
| `request_body` | string? | UTF-8 decoded body; omitted when empty |
| `response_body` | string? | UTF-8 decoded body; omitted when empty |
| `source_addr` | string? | Client socket address, e.g. `"127.0.0.1:54321"` |
| `dest_addr` | string? | Server socket address, e.g. `"93.184.216.34:443"` |

---

## Testing

### Run All Tests

```sh
cargo test --workspace --all-targets --all-features
# or simply:
make test
```

### Run Tests for a Single Crate

```sh
cargo test -p phantom-storage
cargo test -p phantom-core
cargo test -p phantom-tui
```

### Run a Specific Integration Test

```sh
# Node.js proxy integration tests (requires node + npm in PATH)
cargo test --test proxy_node_integration -- --nocapture

# Single test function:
cargo test --test proxy_node_integration test_proxy_captures_node_app_traffic -- --nocapture
cargo test --test proxy_node_integration test_proxy_captures_alternative_http_clients -- --nocapture
```

### Node.js Integration Tests (`tests/proxy_node_integration.rs`)

| Test | Description |
|------|-------------|
| `test_proxy_captures_node_app_traffic` | HTTP + HTTPS GET/POST via `http`/`https` modules (4 traces) |
| `test_proxy_captures_alternative_http_clients` | axios, undici, fetch — HTTP + HTTPS × 3 clients (6 traces) |

Both tests:
- Auto-skip if `node` or `npm` is not in `PATH`.
- Start in-process Rust mock backends (HTTP + HTTPS with self-signed cert).
- Run `phantom --output jsonl --insecure -- node <script>.js`.
- Parse JSONL output and assert method, path, status code per trace.
- Identify traces by `x-phantom-client` custom header in `request_headers`.

### Run a Single Unit Test Function

```sh
# By substring match (simplest):
cargo test test_insert_and_get

# By crate + substring:
cargo test -p phantom-storage test_insert_and_get

# By fully-qualified path:
cargo test -p phantom-storage fjall_store::tests::test_insert_and_get
```

### Testing Conventions

- Tests live in inline `#[cfg(test)]` modules at the bottom of the implementation file (not separate files).
- Each test creates an isolated `tempfile::tempdir()` — never share mutable state between tests.
- Test fixture factories follow the pattern `fn make_<type>(args) -> Type { ... }`.
- Test function names use `test_<what_is_being_tested>` in `snake_case`.
- `unwrap()` and `expect()` are acceptable inside test code.
- Dev dependencies (`tempfile`, `rand`) go in the per-crate `[dev-dependencies]`, not the workspace root.
- Run the full test suite before submitting any change: `cargo test --workspace`.

---

## Code Style

### Rust Edition and Toolchain

- Edition: **2024** (`edition = "2024"` in all `Cargo.toml` files).
- No `rust-toolchain.toml` or `rustfmt.toml` — use the default stable toolchain and default `rustfmt` settings.
- All `cargo fmt` and `cargo clippy` output must be clean. CI runs `clippy -- -D warnings`.

### Naming Conventions

| Item | Convention | Examples |
|---|---|---|
| Files / modules | `snake_case` | `fjall_store.rs`, `phantom_core` |
| Structs | `PascalCase` | `HttpTrace`, `FjallTraceStore` |
| Enums | `PascalCase` | `HttpMethod`, `StorageError` |
| Enum variants | `PascalCase` | `HttpMethod::Get`, `Pane::TraceList` |
| Traits | `PascalCase` | `TraceStore`, `CaptureBackend` |
| Functions / methods | `snake_case` | `list_recent`, `render_trace_list` |
| Local variables | `snake_case` | `trace_rx`, `span_id` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_BODY_SIZE` |
| Generic type params | `PascalCase` | `T`, `E`, `Store` |

### Import Organization

Use three groups separated by blank lines, in this order:

1. `std` imports
2. External crate imports (alphabetical)
3. Internal crate imports (`crate::`, `phantom_core::`, etc.)

```rust
use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use phantom_core::{CaptureError, HttpTrace};
```

### Formatting

- Indentation: **4 spaces** (rustfmt default).
- Trailing commas on multi-line struct literals, enum variants, and function argument lists.
- Prefer explicit `match` over chains of `if let` when exhaustiveness matters.
- Keep lines to roughly 100 characters; longer lines are tolerated in UI rendering code.
- Section dividers use `// ──────...` comment style (see existing files for width).

### Types and Generics

- Use **newtype wrappers** for domain identifiers: `struct TraceId(pub [u8; 16])`.
  Implement `Display`, `Debug`, `Serialize`, `Deserialize` on each newtype.
- Use **const generics** for fixed-size byte utilities: `fn rand_bytes<const N: usize>() -> [u8; N]`.
- Avoid `Box<dyn Error>` in library code — use typed errors (see Error Handling below).
- Prefer `impl Trait` in function return position only when the concrete type is unambiguous and the trait is simple.

### Error Handling

Two-tier strategy — choose based on whether the code is a library crate or the binary:

**Library crates (`crates/*`):**
- Use `thiserror` — define enums with `#[derive(thiserror::Error)]`.
- Each variant carries a `#[error("...")]` message describing the failure.
- Do not use `anyhow` inside library crates.

```rust
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("write error: {0}")]
    Write(#[from] fjall::Error),
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}
```

**Binary / application layer (`src/main.rs`, TUI wiring):**
- Use `anyhow::Result<()>` for `main()` and top-level wiring.
- Convert library errors with `.map_err(|e| anyhow::anyhow!("{e}"))` or `?` when `From` is implemented.

**General rules:**
- Use the `?` operator for propagation; avoid `.unwrap()` in non-test production code.
- `unwrap()` / `expect("reason")` are only acceptable in: test code, and places where failure is a programming error (e.g., initializing a static CA cert).
- Log dropped channel messages with `tracing::warn!` rather than propagating the error.

### Async Patterns

- Async runtime: **Tokio** (`features = ["full"]`).
- The storage layer (Fjall) is **synchronous**. Never block the Tokio executor with synchronous storage calls from an async context — run them on a blocking thread or keep them in the TUI tick loop.
- Use `mpsc::try_recv()` (non-blocking) to drain the capture channel on each TUI tick rather than `.await`-ing inside the render loop.
- Channels (`mpsc`, `oneshot`) are the primary mechanism for crossing the async/sync boundary.

### Platform-Specific Code

- `phantom-agent` and `LdPreloadCaptureBackend` are **Linux-only**. Gate with `#[cfg(target_os = "linux")]` at the module and item level.
- `phantom-agent` is a `dylib` crate — it has no workspace crate dependencies to avoid symbol conflicts.
- IPC between agent and main process uses Unix datagram sockets (`PHANTOM_SOCKET` env var). Max datagram: 60 KB.

### Architecture Conventions

- **`phantom-core` is the only source of shared types.** Do not define domain types in leaf crates.
- **Share state with `Arc<dyn Trait>`.** Never pass concrete storage or capture types across component boundaries — always use the trait object form (e.g., `Arc<dyn TraceStore>`).
- **Storage design:** Fjall partitions — `traces` (primary KV), `by_time` (timestamp prefix index), `by_trace_id` (trace ID prefix index). New indices follow the same `{index_key || span_id} → span_id` pattern.
- **TUI state:** All mutable state lives in `App`. Rendering functions are pure (`fn render_*(frame, app)`) and must not mutate `App`.
- **W3C Trace Context:** `TraceId` is 128-bit, `SpanId` is 64-bit. Preserve this for distributed tracing compatibility.

---

## Key Files Reference

| File | Purpose |
|---|---|
| `src/main.rs` | CLI parsing (`clap` derive), backend/output mode selection, child-process spawning, JSONL output loop |
| `tests/proxy_node_integration.rs` | Integration tests: Node.js proxy capture, alternative HTTP client tracing |
| `tests/apps/node-app/proxy-preload.js` | Node.js `--require` preload: patches `http`, `https`, `undici`, `fetch` to go through proxy |
| `tests/apps/node-app/client.js` | Test client: `http`/`https` module usage (basic integration test) |
| `tests/apps/node-app/client-alts.js` | Test client: `axios`, `undici`, `globalThis.fetch` (alternative HTTP clients test) |
| `tests/apps/node-app/package.json` | Node app deps: `axios ^1.7`, `undici ^7` |
| `crates/phantom-core/src/trace.rs` | `HttpTrace`, `TraceId`, `SpanId`, `HttpMethod` |
| `crates/phantom-core/src/storage.rs` | `TraceStore` trait |
| `crates/phantom-core/src/capture.rs` | `CaptureBackend` trait |
| `crates/phantom-core/src/error.rs` | `CaptureError`, `StorageError` |
| `crates/phantom-storage/src/fjall_store.rs` | Storage implementation + all storage tests |
| `crates/phantom-capture/src/proxy.rs` | MITM proxy implementation (cross-platform) |
| `crates/phantom-capture/src/ldpreload.rs` | LD_PRELOAD capture backend (Linux only) |
| `crates/phantom-agent/src/lib.rs` | LD_PRELOAD dylib: hooks libc `send`/`recv`/`close` |
| `crates/phantom-tui/src/app.rs` | TUI state (`App`) and all state mutation |
| `crates/phantom-tui/src/ui.rs` | Ratatui rendering functions |
| `crates/phantom-tui/src/lib.rs` | TUI entry point and event loop |
| `crates/phantom-tui/src/event.rs` | `EventHandler`: crossterm key events + tick |
| `plan.md` | Comprehensive technical design (Japanese) |

---

## Data Flow

```
HTTP traffic
  → proxy.rs TraceHandler::handle_request()   # stores PendingRequest on self
  → proxy.rs TraceHandler::handle_response()  # builds HttpTrace, try_send to mpsc
  → lib.rs TUI loop try_recv()                # drains channel each tick
  → fjall_store.rs FjallTraceStore::insert()  # batch write: traces + by_time + by_trace_id
  → app.rs App::add_trace()                   # prepends to traces Vec, bumps count
  → ui.rs render()                            # pure read of App state, no mutation

Node.js auto-injection (proxy mode, -- node app.js):
  → main.rs spawn_proxy_child()               # writes proxy-preload.js to /tmp, prepends --require
  → proxy-preload.js loaded by Node at startup
  → patches http.request, https.request, undici dispatcher, globalThis.fetch
  → all outbound Node requests → phantom MITM proxy

LD_PRELOAD flow (Linux only):
  → phantom-agent dylib hooks send()/recv()   # intercepts plain-text HTTP/1.x
  → sends JSON datagrams over UnixDatagram    # PHANTOM_SOCKET env var
  → ldpreload.rs LdPreloadCaptureBackend      # receives, parses, emits HttpTrace
  → (same mpsc channel as proxy flow above)
```

**Channel capacity:** 4096. Dropped traces logged via `tracing::warn!`.
**Storage on startup:** `list_recent(1000, 0)` loads existing traces into `App` before event loop.

---

## Dependency Management

- Shared dependencies are declared once in the `[workspace.dependencies]` table in the root `Cargo.toml` and referenced with `{ workspace = true }` in crate manifests.
- When adding a new dependency, check `[workspace.dependencies]` first. If it is already there, use `workspace = true` rather than a separate version pin.
- Prefer pure-Rust crates when available (e.g., Fjall over RocksDB, rcgen over OpenSSL-based cert generation).
- `phantom-agent` intentionally pins its own dependency versions (no `workspace = true`) to stay self-contained as a dylib.

---

## Crate AGENTS.md Index

| Crate | AGENTS.md | Key Focus |
|-------|-----------|-----------|
| `phantom-core` | `crates/phantom-core/AGENTS.md` | Types, traits, error enums |
| `phantom-storage` | `crates/phantom-storage/AGENTS.md` | Fjall partitions, index design |
| `phantom-capture` | `crates/phantom-capture/AGENTS.md` | Hudsucker proxy, HTTPS interception, LD_PRELOAD backend |
| `phantom-tui` | `crates/phantom-tui/AGENTS.md` | Ratatui loop, App state, rendering |
