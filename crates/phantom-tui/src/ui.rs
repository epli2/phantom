use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{App, Pane};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Status bar
            Constraint::Min(0),   // Main area
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_main(frame, app, chunks[1]);
    render_help_bar(frame, app, chunks[2]);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = Line::from(vec![
        Span::styled(" phantom", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" v0.1.0 | "),
        Span::styled(
            format!("Traces: {}", app.trace_count),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" | Capturing via "),
        Span::styled(&app.backend_name, Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(
        Paragraph::new(status).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn render_main(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    render_trace_list(frame, app, chunks[0]);
    render_trace_detail(frame, app, chunks[1]);
}

fn render_trace_list(frame: &mut Frame, app: &App, area: Rect) {
    let list_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Filter bar
    let filter_style = if app.filter_active {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let filter_text = if app.filter.is_empty() && !app.filter_active {
        "Press / to filter".to_string()
    } else {
        app.filter.clone()
    };
    let filter_block = Block::default()
        .borders(Borders::ALL)
        .border_style(filter_style)
        .title(" Filter ");
    let filter = Paragraph::new(filter_text).block(filter_block);
    frame.render_widget(filter, list_chunks[0]);

    // Trace table
    let filtered = app.filtered_traces();

    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("Method"),
        Cell::from("URL"),
        Cell::from("Status"),
        Cell::from("Duration"),
    ])
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(i, trace)| {
            let time = format_time(&trace.timestamp);
            let method = trace.method.to_string();
            let url = truncate_url(&trace.url, 30);
            let status = trace.status_code.to_string();
            let dur = format!("{:.0?}", trace.duration);

            let status_color = match trace.status_code {
                200..=299 => Color::Green,
                300..=399 => Color::Yellow,
                400..=499 => Color::Red,
                500..=599 => Color::Magenta,
                _ => Color::White,
            };

            let style = if i == app.selected_index {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(time),
                Cell::from(method).style(Style::default().fg(Color::Cyan)),
                Cell::from(url),
                Cell::from(status).style(Style::default().fg(status_color)),
                Cell::from(dur).style(Style::default().fg(Color::DarkGray)),
            ])
            .style(style)
        })
        .collect();

    let border_style = if app.active_pane == Pane::TraceList {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(6),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(format!(" Traces ({}) ", filtered.len())),
    );

    let mut state = TableState::default();
    state.select(Some(app.selected_index));
    frame.render_stateful_widget(table, list_chunks[1], &mut state);
}

fn render_trace_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.active_pane == Pane::TraceDetail {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Detail ");

    let Some(trace) = app.selected_trace() else {
        let empty = Paragraph::new("No trace selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // Request section
    lines.push(Line::from(vec![
        Span::styled("Request", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(trace.method.to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::raw(&trace.url),
    ]));
    lines.push(Line::from(""));

    // Request headers
    for (key, value) in &trace.request_headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{key}: "), Style::default().fg(Color::Yellow)),
            Span::raw(truncate_str(value, 60)),
        ]));
    }

    // Request body
    if let Some(body) = &trace.request_body {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Body:",
            Style::default().fg(Color::DarkGray),
        )));
        append_body_lines(&mut lines, body);
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("━".repeat(40), Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(""));

    // Response section
    let status_color = match trace.status_code {
        200..=299 => Color::Green,
        300..=399 => Color::Yellow,
        400..=499 => Color::Red,
        500..=599 => Color::Magenta,
        _ => Color::White,
    };
    lines.push(Line::from(vec![
        Span::styled("Response", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" ("),
        Span::styled(
            trace.status_code.to_string(),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(", {:.0?})", trace.duration)),
    ]));
    lines.push(Line::from(""));

    // Response headers
    for (key, value) in &trace.response_headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{key}: "), Style::default().fg(Color::Yellow)),
            Span::raw(truncate_str(value, 60)),
        ]));
    }

    // Response body
    if let Some(body) = &trace.response_body {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Body:",
            Style::default().fg(Color::DarkGray),
        )));
        append_body_lines(&mut lines, body);
    }

    let detail = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn render_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let help = if app.filter_active {
        Line::from(vec![
            Span::styled(" [Esc]", Style::default().fg(Color::Yellow)),
            Span::raw("cancel  "),
            Span::styled("[Enter]", Style::default().fg(Color::Yellow)),
            Span::raw("apply  "),
            Span::styled("[Backspace]", Style::default().fg(Color::Yellow)),
            Span::raw("delete"),
        ])
    } else {
        Line::from(vec![
            Span::styled(" [q]", Style::default().fg(Color::Yellow)),
            Span::raw("uit  "),
            Span::styled("[/]", Style::default().fg(Color::Yellow)),
            Span::raw("filter  "),
            Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
            Span::raw("navigate  "),
            Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
            Span::raw("switch  "),
            Span::styled("[g/G]", Style::default().fg(Color::Yellow)),
            Span::raw("top/bottom"),
        ])
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn format_time(ts: &std::time::SystemTime) -> String {
    let duration = ts
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let hours = (secs / 3600) % 24;
    let minutes = (secs / 60) % 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn truncate_url(url: &str, max_len: usize) -> String {
    // Strip scheme for display
    let display = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    if display.len() > max_len {
        format!("{}…", &display[..max_len - 1])
    } else {
        display.to_string()
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}…", &s[..max_len - 1])
    } else {
        s.to_string()
    }
}

fn append_body_lines(lines: &mut Vec<Line>, body: &[u8]) {
    if let Ok(text) = std::str::from_utf8(body) {
        // Try pretty-printing JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                for line in pretty.lines().take(30) {
                    lines.push(Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::White),
                    )));
                }
                return;
            }
        }
        // Plain text
        for line in text.lines().take(30) {
            lines.push(Line::from(line.to_string()));
        }
    } else {
        lines.push(Line::from(Span::styled(
            format!("<binary, {} bytes>", body.len()),
            Style::default().fg(Color::DarkGray),
        )));
    }
}
