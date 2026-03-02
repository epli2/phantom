use phantom_core::mysql::MysqlResponseKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{ActiveTab, App, Pane};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Status bar
            Constraint::Length(1), // Tab bar
            Constraint::Min(0),    // Main area
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_tab_bar(frame, app, chunks[1]);
    render_main(frame, app, chunks[2]);
    render_help_bar(frame, app, chunks[3]);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status = Line::from(vec![
        Span::styled(
            " phantom",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" v0.1.0 | "),
        Span::styled(
            format!("HTTP: {}", app.trace_count),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" | "),
        Span::styled(
            format!("MySQL: {}", app.mysql_trace_count),
            Style::default().fg(Color::Blue),
        ),
        Span::raw(" | Capturing via "),
        Span::styled(&app.backend_name, Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(
        Paragraph::new(status).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let http_style = if app.active_tab == ActiveTab::Http {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mysql_style = if app.active_tab == ActiveTab::Mysql {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let tab_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(" [1] HTTP ", http_style),
        Span::raw("  "),
        Span::styled(" [2] MySQL ", mysql_style),
    ]);
    frame.render_widget(Paragraph::new(tab_line), area);
}

fn render_main(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    match app.active_tab {
        ActiveTab::Http => {
            render_trace_list(frame, app, chunks[0]);
            render_trace_detail(frame, app, chunks[1]);
        }
        ActiveTab::Mysql => {
            render_mysql_list(frame, app, chunks[0]);
            render_mysql_detail(frame, app, chunks[1]);
        }
    }
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
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );

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
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
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
    lines.push(Line::from(vec![Span::styled(
        "Request",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![
        Span::styled(
            trace.method.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
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
    lines.push(Line::from(vec![Span::styled(
        "━".repeat(40),
        Style::default().fg(Color::DarkGray),
    )]));
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
        Span::styled(
            "Response",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ("),
        Span::styled(
            trace.status_code.to_string(),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
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
            Span::styled("[1/2]", Style::default().fg(Color::Yellow)),
            Span::raw("tab  "),
            Span::styled("[/]", Style::default().fg(Color::Yellow)),
            Span::raw("filter  "),
            Span::styled("[j/k]", Style::default().fg(Color::Yellow)),
            Span::raw("navigate  "),
            Span::styled("[Tab]", Style::default().fg(Color::Yellow)),
            Span::raw("pane  "),
            Span::styled("[g/G]", Style::default().fg(Color::Yellow)),
            Span::raw("top/bottom"),
        ])
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// MySQL tab rendering
// ─────────────────────────────────────────────────────────────────────────────

fn render_mysql_list(frame: &mut Frame, app: &App, area: Rect) {
    let list_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Filter bar (same as HTTP tab)
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
    frame.render_widget(
        Paragraph::new(filter_text).block(filter_block),
        list_chunks[0],
    );

    // MySQL trace table
    let filtered = app.filtered_mysql_traces();

    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("Query"),
        Cell::from("Result"),
        Cell::from("Duration"),
    ])
    .style(
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(i, trace)| {
            let time = format_time(&trace.timestamp);
            let query = truncate_str(&trace.query, 35);
            let (result_str, result_color) = format_mysql_response(&trace.response);
            let dur = format!("{:.0?}", trace.duration);

            let style = if i == app.mysql_selected_index {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(time),
                Cell::from(query),
                Cell::from(result_str).style(Style::default().fg(result_color)),
                Cell::from(dur).style(Style::default().fg(Color::DarkGray)),
            ])
            .style(style)
        })
        .collect();

    let border_style = if app.active_pane == Pane::TraceList {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(15),
            Constraint::Length(18),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(format!(" MySQL Queries ({}) ", filtered.len())),
    );

    let mut state = TableState::default();
    state.select(Some(app.mysql_selected_index));
    frame.render_stateful_widget(table, list_chunks[1], &mut state);
}

fn render_mysql_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.active_pane == Pane::TraceDetail {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" MySQL Detail ");

    let Some(trace) = app.selected_mysql_trace() else {
        let empty = Paragraph::new("No query selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // Query
    lines.push(Line::from(Span::styled(
        "Query",
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    for query_line in trace.query.lines() {
        lines.push(Line::from(Span::styled(
            query_line.to_string(),
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "━".repeat(40),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    // Response
    lines.push(Line::from(Span::styled(
        "Result",
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    match &trace.response {
        MysqlResponseKind::Ok {
            affected_rows,
            last_insert_id,
            warnings,
        } => {
            lines.push(Line::from(vec![
                Span::styled(
                    "OK",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {affected_rows} row(s) affected")),
            ]));
            if *last_insert_id > 0 {
                lines.push(Line::from(format!("  last_insert_id = {last_insert_id}")));
            }
            if *warnings > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  {warnings} warning(s)"),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }
        MysqlResponseKind::ResultSet {
            column_count,
            row_count,
        } => {
            lines.push(Line::from(vec![
                Span::styled(
                    "ResultSet",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  {column_count} col(s), {row_count} row(s)")),
            ]));
        }
        MysqlResponseKind::Err {
            error_code,
            sql_state,
            message,
        } => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("ERR {error_code}"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  ({sql_state})")),
            ]));
            lines.push(Line::from(Span::styled(
                message.clone(),
                Style::default().fg(Color::Red),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "━".repeat(40),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(""));

    // Metadata
    lines.push(Line::from(Span::styled(
        "Metadata",
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!("  Duration:  {:.0?}", trace.duration)));
    lines.push(Line::from(format!(
        "  Timestamp: {}",
        format_time(&trace.timestamp)
    )));
    if let Some(addr) = &trace.dest_addr {
        lines.push(Line::from(format!("  Server:    {addr}")));
    }
    if let Some(db) = &trace.db_name {
        lines.push(Line::from(format!("  Database:  {db}")));
    }

    let detail = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

/// Format a `MysqlResponseKind` as a short display string and its color.
fn format_mysql_response(response: &MysqlResponseKind) -> (String, Color) {
    match response {
        MysqlResponseKind::Ok { affected_rows, .. } => {
            (format!("OK {affected_rows} row(s)"), Color::Cyan)
        }
        MysqlResponseKind::ResultSet {
            column_count,
            row_count,
        } => (
            format!("{column_count} cols, {row_count} rows"),
            Color::Green,
        ),
        MysqlResponseKind::Err { error_code, .. } => (format!("ERR {error_code}"), Color::Red),
    }
}

fn format_time(ts: &std::time::SystemTime) -> String {
    let duration = ts.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
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
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text)
            && let Ok(pretty) = serde_json::to_string_pretty(&json)
        {
            for line in pretty.lines().take(30) {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
            return;
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
