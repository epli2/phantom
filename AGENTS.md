# Phantom — Agent Instructions

Phantom is a Rust-based API observability tool that captures HTTP/HTTPS traffic via a MITM proxy and presents it in a terminal UI. It is organized as a Cargo workspace of four library crates plus one binary.

---

## Project Layout

```
src/main.rs                  # Binary entry point: CLI parsing, component wiring
crates/
  phantom-core/              # Domain types, traits, error types — no I/O
  phantom-storage/           # Fjall LSM-tree TraceStore implementation
  phantom-capture/           # Hudsucker MITM proxy CaptureBackend
  phantom-tui/               # Ratatui terminal UI
Cargo.toml                   # Workspace root + binary crate
plan.md                      # Japanese-language technical design document
```

**Dependency graph** (no circular dependencies):
```
main → phantom-capture → phantom-core
main → phantom-storage → phantom-core
main → phantom-tui    → phantom-core
```

`phantom-core` has zero internal-crate dependencies. All cross-component sharing is done via `Arc<dyn TraitFromPhantomCore>`.

---

## Build, Run, and Check Commands

```sh
cargo build                  # Debug build
cargo build --release        # Release build
cargo run                    # Run proxy on default port 8080
cargo run -- --port 9090     # Run with custom port
cargo check                  # Fast type/borrow check (no codegen)
cargo clippy                 # Lint (follow all suggestions)
cargo fmt                    # Format all code
```

### CLI Options

| Flag | Default | Description |
|---|---|---|
| `-p, --port <PORT>` | `8080` | Proxy capture port |
| `-d, --data-dir <DIR>` | `~/.local/share/phantom/data` | Storage directory |

---

## Testing

### Run All Tests

```sh
cargo test --workspace
```

### Run Tests for a Single Crate

```sh
cargo test -p phantom-storage
cargo test -p phantom-core
cargo test -p phantom-tui
```

### Run a Single Test Function

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
- All `cargo fmt` and `cargo clippy` output must be clean.

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
| `src/main.rs` | CLI parsing (`clap` derive), component wiring |
| `crates/phantom-core/src/trace.rs` | `HttpTrace`, `TraceId`, `SpanId`, `HttpMethod` |
| `crates/phantom-core/src/storage.rs` | `TraceStore` trait |
| `crates/phantom-core/src/capture.rs` | `CaptureBackend` trait |
| `crates/phantom-core/src/error.rs` | `CaptureError`, `StorageError` |
| `crates/phantom-storage/src/fjall_store.rs` | Storage implementation + all storage tests |
| `crates/phantom-capture/src/proxy.rs` | MITM proxy implementation |
| `crates/phantom-tui/src/app.rs` | TUI state (`App`) and all state mutation |
| `crates/phantom-tui/src/ui.rs` | Ratatui rendering functions |
| `crates/phantom-tui/src/lib.rs` | TUI entry point and event loop |
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
```

**Channel capacity:** 4096. Dropped traces logged via `tracing::warn!`.
**Storage on startup:** `list_recent(1000, 0)` loads existing traces into `App` before event loop.

---

## Crate AGENTS.md Index

| Crate | AGENTS.md | Key Focus |
|-------|-----------|-----------|
| `phantom-core` | `crates/phantom-core/AGENTS.md` | Types, traits, error enums |
| `phantom-storage` | `crates/phantom-storage/AGENTS.md` | Fjall partitions, index design |
| `phantom-capture` | `crates/phantom-capture/AGENTS.md` | Hudsucker proxy, HTTPS interception |
| `phantom-tui` | `crates/phantom-tui/AGENTS.md` | Ratatui loop, App state, rendering |

## Dependency Management

- Shared dependencies are declared once in the `[workspace.dependencies]` table in the root `Cargo.toml` and referenced with `{ workspace = true }` in crate manifests.
- When adding a new dependency, check `[workspace.dependencies]` first. If it is already there, use `workspace = true` rather than a separate version pin.
- Prefer pure-Rust crates when available (e.g., Fjall over RocksDB, rcgen over OpenSSL-based cert generation).
