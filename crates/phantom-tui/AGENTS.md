# phantom-tui — Agent Instructions

**Role:** Ratatui terminal UI. Event loop, state management, rendering. Async entry point, sync render/state mutation.

---

## STRUCTURE

```
crates/phantom-tui/src/
├── lib.rs       # run_tui() async entry point + key handlers
├── app.rs       # App struct + all state mutation methods
├── ui.rs        # Pure rendering functions (fn render_*())
├── event.rs     # EventHandler (crossterm events + tick)
└── components/  # (empty, reserved for future extraction)
```

---

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| Add keyboard shortcut | `lib.rs:79` | `handle_normal_key()` or `handle_filter_key()` |
| Add App state field | `app.rs:9` | `App` struct + init in `App::new()` |
| Add state mutation method | `app.rs:20` | `impl App` block |
| Change layout / add panel | `ui.rs:9` | `render()` → `render_main()` → split panels |
| Add new rendering section | `ui.rs` | New `fn render_*()`, call from `render_main()` |
| Change event tick rate | `lib.rs:39` | `EventHandler::new(50)` — 50ms |

---

## App STATE (app.rs)

| Field | Type | Purpose |
|-------|------|---------|
| `traces` | `Vec<HttpTrace>` | All traces, newest first (insert at index 0) |
| `selected_index` | `usize` | Index into `filtered_traces()` |
| `filter` | `String` | URL substring filter text |
| `filter_active` | `bool` | Filter input mode |
| `active_pane` | `Pane` | `TraceList` or `TraceDetail` |
| `should_quit` | `bool` | Main loop exit signal |
| `trace_count` | `u64` | Total ever captured (includes filtered-out) |
| `backend_name` | `String` | Shown in status bar |

---

## MAIN LOOP (lib.rs)

```
loop:
  1. terminal.draw(render)          // pure render, no mutation
  2. trace_rx.try_recv() loop       // drain channel + store.insert + app.add_trace
  3. events.poll()                  // key event or tick
  4. handle_*_key(app, ...)         // mutate app state
  5. if app.should_quit { break }
```

**Critical:** `try_recv()` is non-blocking. Never `.await` channel inside render loop.

---

## RENDERING CONVENTIONS (ui.rs)

- All render functions are **pure**: `fn render_*(frame: &mut Frame, app: &App, area: Rect)`. No `&mut App`.
- Layout: 3-row vertical split (1 status bar | min main | 1 help bar). Main splits horizontal 45%/55% (list/detail).
- Trace list uses `TableState` with `state.select(Some(app.selected_index))` — recreated each frame.
- Status colors: 2xx=Green, 3xx=Yellow, 4xx=Red, 5xx=Magenta, other=White.
- Body rendering: tries JSON pretty-print first, falls back to plain text, then `<binary, N bytes>`. Capped at 30 lines.
- URL display strips `http://`/`https://` prefix, truncates at 30 chars with `…`.
- Active pane has `Color::Cyan` border; inactive has `Color::DarkGray`.

## ANTI-PATTERNS

- Do NOT mutate `App` inside any `render_*` function.
- Do NOT `.await` inside the main loop body — use `try_recv()`, not `recv().await`.
- Do NOT call `store.insert()` from async context with blocking — currently in sync tick loop (acceptable).
- Do NOT add `anyhow` to this crate — use `std::io::Result<()>` for TUI I/O errors; `phantom-core` errors for storage.
- Do NOT share `TableState` in `App` — it is ephemeral, created per frame.
