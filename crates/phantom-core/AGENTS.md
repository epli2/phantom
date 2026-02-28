# phantom-core — Agent Instructions

**Role:** Shared domain types and traits. Zero I/O. Zero internal-crate deps. The single source of truth for all cross-component data structures.

---

## STRUCTURE

```
crates/phantom-core/src/
├── lib.rs        # Re-exports: pub mod trace, capture, storage, error
├── trace.rs      # HttpTrace, TraceId, SpanId, HttpMethod
├── storage.rs    # TraceStore trait
├── capture.rs    # CaptureBackend trait
└── error.rs      # CaptureError, StorageError (thiserror)
```

---

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add/change HTTP trace fields | `trace.rs` | HttpTrace struct |
| Add storage query method | `storage.rs` | TraceStore trait |
| Add capture mode | `capture.rs` | CaptureBackend trait |
| Add error variant | `error.rs` | Two enums: CaptureError, StorageError |

---

## CODE MAP

| Symbol | Kind | Location |
|--------|------|----------|
| `HttpTrace` | struct | `trace.rs:79` |
| `TraceId` | newtype `[u8;16]` | `trace.rs:9` |
| `SpanId` | newtype `[u8;8]` | `trace.rs:28` |
| `HttpMethod` | enum (9 variants) | `trace.rs:48` |
| `TraceStore` | trait | `storage.rs:5` |
| `CaptureBackend` | trait | `capture.rs:10` |
| `StorageError` | enum | `error.rs:14` |
| `CaptureError` | enum | `error.rs:4` |

---

## CONVENTIONS

- **Newtypes** for all IDs: `struct TraceId(pub [u8; 16])`. Implement `Display` (hex), `Debug`, `Serialize`, `Deserialize`, `Clone`, `PartialEq`, `Eq`, `Hash`.
- **`TraceStore` is `Send + Sync`** — it crosses the async/sync boundary via `Arc<dyn TraceStore>`.
- **`CaptureBackend` is `Send`** only — started on main thread, hands off a channel.
- **`StorageError` variants** use `String` wrapping (not `#[from]`) to keep the crate dep-free from storage libs.
- `HttpTrace` derives `Serialize, Deserialize` — field names are the serialization contract. Rename with care.

## ANTI-PATTERNS

- Do NOT add `anyhow` or any I/O dependency here.
- Do NOT add `impl` logic that touches files, network, or tokio — this crate is pure domain.
- Do NOT define new domain types in leaf crates — always add them here.
- Do NOT put `Arc` or wiring code here — that belongs in `main.rs`.
- `TraceStore` methods are **sync** by design (Fjall is sync). Do not add `async fn` to the trait.
