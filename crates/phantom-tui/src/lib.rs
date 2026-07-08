mod app;
mod event;
mod export;
mod ui;

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{event::KeyEventKind, execute};
use phantom_core::storage::TraceStore;
use phantom_core::trace::HttpTrace;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::{App, Pane};
use crate::event::{Event, EventHandler};

pub async fn run_tui(
    store: Arc<dyn TraceStore>,
    mut trace_rx: mpsc::Receiver<HttpTrace>,
    backend_name: &str,
) -> std::io::Result<()> {
    // Initialize terminal
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(backend_name);

    // Load existing traces from storage
    if let Ok(existing) = store.list_recent(1000, 0) {
        app.trace_count = existing.len() as u64;
        app.traces = existing;
    }

    let events = EventHandler::new(50); // 50ms tick

    loop {
        // Draw UI
        terminal.draw(|frame| ui::render(frame, &app))?;

        // Drain all pending traces from the channel (non-blocking)
        while let Ok(trace) = trace_rx.try_recv() {
            let _ = store.insert(&trace);
            app.add_trace(trace);
        }

        // Handle events
        match events.poll()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                app.clear_status_message();
                if app.help_visible {
                    handle_help_key(&mut app, key.code);
                } else if app.filter_active {
                    handle_filter_key(&mut app, key.code);
                } else {
                    handle_normal_key(&mut app, key.code, key.modifiers);
                }
            }
            Event::Tick => {}
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if app.active_pane == Pane::TraceDetail {
                app.scroll_detail_down();
            } else {
                app.move_down();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.active_pane == Pane::TraceDetail {
                app.scroll_detail_up();
            } else {
                app.move_up();
            }
        }
        KeyCode::Char('g') | KeyCode::Home => app.jump_top(),
        KeyCode::Char('G') | KeyCode::End => app.jump_bottom(),
        KeyCode::Tab => app.toggle_pane(),
        KeyCode::Char('[') => app.prev_detail_tab(),
        KeyCode::Char(']') => app.next_detail_tab(),
        KeyCode::Char('/') => app.activate_filter(),
        KeyCode::Char('?') => app.toggle_help(),
        KeyCode::Char('c') => copy_curl_to_clipboard(app),
        KeyCode::Char('w') => write_trace_to_file(app),
        KeyCode::Esc => app.clear_filter(),
        _ => {}
    }
}

fn handle_filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.deactivate_filter(),
        KeyCode::Enter => app.deactivate_filter(),
        KeyCode::Backspace => app.pop_filter_char(),
        KeyCode::Char(c) => app.push_filter_char(c),
        _ => {}
    }
}

fn handle_help_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('?') => app.close_help(),
        _ => {}
    }
}

/// Copy the selected trace as a `curl` command to the system clipboard (`c`
/// key). Falls back to printing on stderr if no clipboard is available
/// (e.g. headless Linux with no X11/Wayland session).
fn copy_curl_to_clipboard(app: &mut App) {
    let cmd = app.selected_trace().map(export::trace_to_curl);
    let Some(cmd) = cmd else {
        app.set_status_message("No trace selected");
        return;
    };
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(cmd.clone())) {
        Ok(()) => app.set_status_message("Copied curl command to clipboard"),
        Err(e) => {
            eprintln!("{cmd}");
            app.set_status_message(format!("Clipboard unavailable ({e}) — printed to stderr"));
        }
    }
}

/// Write the selected trace to `./phantom-trace-<span_id>.json` (`w` key).
fn write_trace_to_file(app: &mut App) {
    let data = app.selected_trace().map(|trace| {
        (
            format!("phantom-trace-{}.json", trace.span_id),
            serde_json::to_string_pretty(trace),
        )
    });
    let Some((filename, json_result)) = data else {
        app.set_status_message("No trace selected");
        return;
    };
    match json_result {
        Ok(json) => match std::fs::write(&filename, json) {
            Ok(()) => app.set_status_message(format!("Wrote {filename}")),
            Err(e) => app.set_status_message(format!("Failed to write {filename}: {e}")),
        },
        Err(e) => app.set_status_message(format!("Failed to serialize trace: {e}")),
    }
}
