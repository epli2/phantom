# phantom-capture — Agent Instructions

**Role:** Hudsucker MITM proxy implementation of `CaptureBackend`. Async (Tokio). HTTPS interception via rcgen CA.

---

## STRUCTURE

```
crates/phantom-capture/src/
├── lib.rs      # pub use proxy::ProxyCaptureBackend
└── proxy.rs    # ProxyCaptureBackend, TraceHandler, helpers
```

---

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Capture lifecycle (start/stop) | `proxy.rs:35` | `impl CaptureBackend for ProxyCaptureBackend` |
| Request → response correlation | `proxy.rs:132` | `impl HttpHandler for TraceHandler` |
| CA cert generation | `proxy.rs:88` | `generate_ca()` — uses rcgen, `expect()` acceptable here |
| Body size limit | `proxy.rs:17` | `MAX_BODY_SIZE = 1MB` constant |
| URL reconstruction | `proxy.rs:219` | `reconstruct_url()` — handles proxy-form URIs |

---

## DATA FLOW

```
HTTP request → handle_request() → stores PendingRequest on self.pending
HTTP response → handle_response() → reads self.pending, builds HttpTrace → trace_tx.try_send()
TUI loop → trace_rx.try_recv() → drains channel
```

**`TraceHandler` is `Clone`** — hudsucker clones it per-connection. `pending: Option<PendingRequest>` correlates request to response within one connection.

**Channel capacity:** 4096. On full: `warn!("Trace channel full, dropping trace")` — never panic/error.

**Shutdown:** `oneshot::Sender<()>` → proxy's `with_graceful_shutdown`. The task handle is stored but not awaited on stop (fire-and-forget).

---

## CONVENTIONS

- `rand_bytes::<const N: usize>()` — const generic, used for SpanId (N=8) and TraceId (N=16).
- `collect_body()` truncates at `MAX_BODY_SIZE` — returns `None` for empty bodies.
- `extract_headers()` converts binary header values to `"<binary>"` string.
- `parse_method()` defaults unknown methods to `HttpMethod::Get` (fallback, not error).
- `expect()` is acceptable in `generate_ca()` — programming error if cert generation fails.
- Drop channel errors logged with `tracing::warn!`, never propagated.

## ANTI-PATTERNS

- Do NOT store `Arc<dyn TraceStore>` in `TraceHandler` — capture writes to channel only, storage happens in TUI loop.
- Do NOT add `parent_span_id` support in capture — currently always `None`; requires W3C header parsing.
- Do NOT use `unwrap()` in production async handlers — use `warn!` and continue.
- Do NOT change `TraceHandler` to not be `Clone` — hudsucker requires it.
- Do NOT use `anyhow` — use `CaptureError` from `phantom-core`.
