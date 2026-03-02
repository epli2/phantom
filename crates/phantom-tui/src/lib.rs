mod app;
mod event;
mod ui;

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{event::KeyEventKind, execute};
use phantom_core::mysql::{MysqlStore, MysqlTrace};
use phantom_core::storage::TraceStore;
use phantom_core::trace::HttpTrace;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::{ActiveTab, App};
use crate::event::{Event, EventHandler};

pub async fn run_tui(
    store: Arc<dyn TraceStore>,
    mysql_store: Arc<dyn MysqlStore>,
    mut trace_rx: mpsc::Receiver<HttpTrace>,
    mut mysql_rx: mpsc::Receiver<MysqlTrace>,
    backend_name: &str,
) -> std::io::Result<()> {
    // Initialize terminal
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(backend_name);

    // Load existing HTTP traces from storage
    if let Ok(existing) = store.list_recent(1000, 0) {
        app.trace_count = existing.len() as u64;
        app.traces = existing;
    }

    // Load existing MySQL traces from storage
    if let Ok(existing) = mysql_store.list_recent(1000, 0) {
        app.mysql_trace_count = existing.len() as u64;
        app.mysql_traces = existing;
    }

    let events = EventHandler::new(50); // 50ms tick

    loop {
        // Draw UI
        terminal.draw(|frame| ui::render(frame, &app))?;

        // Drain all pending HTTP traces from the channel (non-blocking)
        while let Ok(trace) = trace_rx.try_recv() {
            let _ = store.insert(&trace);
            app.add_trace(trace);
        }

        // Drain all pending MySQL traces from the channel (non-blocking)
        while let Ok(trace) = mysql_rx.try_recv() {
            let _ = mysql_store.insert(&trace);
            app.add_mysql_trace(trace);
        }

        // Handle events
        match events.poll()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.filter_active {
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
        KeyCode::Char('1') => app.switch_tab(ActiveTab::Http),
        KeyCode::Char('2') => app.switch_tab(ActiveTab::Mysql),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Char('g') | KeyCode::Home => app.jump_top(),
        KeyCode::Char('G') | KeyCode::End => app.jump_bottom(),
        KeyCode::Tab => app.toggle_pane(),
        KeyCode::Char('/') => app.activate_filter(),
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
