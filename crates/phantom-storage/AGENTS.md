# phantom-storage — Agent Instructions

**Role:** Fjall LSM-tree implementation of `TraceStore`. Synchronous only. No async.

---

## STRUCTURE

```
crates/phantom-storage/src/
├── lib.rs           # pub use fjall_store::FjallTraceStore
└── fjall_store.rs   # FjallTraceStore impl + all tests
```

---

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add query method | `fjall_store.rs:70` | `impl TraceStore for FjallTraceStore` block |
| Add new Fjall partition | `fjall_store.rs:8` struct + `open()` | Follow existing pattern |
| Add index key builder | `fjall_store.rs:45-68` | Helper fns `time_key`, `trace_id_key` |
| Tests | `fjall_store.rs:162` | `#[cfg(test)]` module at bottom |

---

## STORAGE DESIGN

**3 Fjall partitions:**

| Partition | Key format | Value | Purpose |
|-----------|-----------|-------|---------|
| `traces` | `span_id (8B)` | JSON-serialized `HttpTrace` | Primary KV store |
| `by_time` | `timestamp_be (8B) \|\| span_id (8B)` | `span_id (8B)` | Reverse-chron listing |
| `by_trace_id` | `trace_id (16B) \|\| span_id (8B)` | `span_id (8B)` | Group spans by trace |

**Index pattern:** `{index_key || span_id} → span_id`. New indices follow this same schema.

**`traces` partition uses KV separation** (`with_kv_separation`) — large values stored out-of-tree. Other partitions use default options.

**Iteration:** `by_time.iter().rev()` for newest-first. Use `.prefix(trace_id.as_bytes())` on `by_trace_id` for trace group lookups.

---

## CONVENTIONS

- All writes are **batched** (`keyspace.batch()` → `.commit()`). Never call `.insert()` directly on individual partitions outside a batch.
- `encode_timestamp` uses **big-endian nanoseconds** — critical for correct lexicographic ordering.
- Error mapping: `.map_err(|e| StorageError::Open(e.to_string()))` pattern throughout. Never use `?` directly on fjall errors (no `From` impl).
- `search_by_url` is a **full scan** (MVP approach, noted in comment). Acceptable for now.
- `count()` uses `approximate_len()` — not exact.

## TEST CONVENTIONS

- `make_trace(url, status)` factory for all tests.
- `rand_bytes_8()` / `rand_bytes_16()` for random IDs — use `rand::random()`.
- Each test: `let dir = tempfile::tempdir().unwrap();` — isolated, never shared.

## ANTI-PATTERNS

- Do NOT call Fjall from async context — it blocks the executor. Keep storage calls in TUI tick or `spawn_blocking`.
- Do NOT add `async fn` to `FjallTraceStore` or `TraceStore` trait.
- Do NOT add domain types here — they belong in `phantom-core`.
- Do NOT use `anyhow` — use `StorageError` from `phantom-core`.
