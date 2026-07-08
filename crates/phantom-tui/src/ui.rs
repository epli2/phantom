use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, DetailTab, Pane};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Status bar
            Constraint::Min(0),    // Main area
            Constraint::Length(1), // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, chunks[0]);
    render_main(frame, app, chunks[1]);
    render_help_bar(frame, app, chunks[2]);

    if app.help_visible {
        render_help_overlay(frame, frame.area());
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            " phantom",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" v0.1.0 | "),
        Span::styled(
            format!("Traces: {}", app.trace_count),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" | Capturing via "),
        Span::styled(&app.backend_name, Style::default().fg(Color::Yellow)),
    ];
    if let Some(msg) = &app.status_message {
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(msg, Style::default().fg(Color::Magenta)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray)),
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

const DETAIL_TABS: [DetailTab; 4] = [
    DetailTab::Request,
    DetailTab::Response,
    DetailTab::Headers,
    DetailTab::Timing,
];

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
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(trace) = app.selected_trace() else {
        let empty = Paragraph::new("No trace selected").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, inner);
        return;
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    render_detail_tab_bar(frame, app, sections[0]);

    let lines = match app.detail_tab {
        DetailTab::Request => request_tab_lines(trace),
        DetailTab::Response => response_tab_lines(trace),
        DetailTab::Headers => headers_tab_lines(trace),
        DetailTab::Timing => timing_tab_lines(trace),
    };
    let total_lines = lines.len();

    let detail = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));
    frame.render_widget(detail, sections[1]);

    if total_lines as u16 > sections[1].height {
        render_scroll_indicator(frame, app, sections[1], total_lines);
    }
}

fn render_detail_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    for tab in DETAIL_TABS {
        let style = if tab == app.detail_tab {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!(" {} ", tab.label()), style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_scroll_indicator(frame: &mut Frame, app: &App, area: Rect, total_lines: usize) {
    let label = format!(" {}/{} ", app.detail_scroll + 1, total_lines);
    let width = label.len() as u16;
    if width >= area.width {
        return;
    }
    let indicator_area = Rect {
        x: area.x + area.width - width,
        y: area.y,
        width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(Color::DarkGray)),
        indicator_area,
    );
}

fn request_tab_lines(trace: &phantom_core::trace::HttpTrace) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            trace.method.to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::raw(trace.url.clone()),
    ]));
    if let Some(body) = &trace.request_body {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Body:",
            Style::default().fg(Color::DarkGray),
        )));
        append_body_lines(
            &mut lines,
            body,
            trace.request_body_truncated,
            trace.request_body_binary,
            trace
                .request_headers
                .get("content-type")
                .map(String::as_str),
        );
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(no request body)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn response_tab_lines(trace: &phantom_core::trace::HttpTrace) -> Vec<Line<'static>> {
    let status_color = match trace.status_code {
        200..=299 => Color::Green,
        300..=399 => Color::Yellow,
        400..=499 => Color::Red,
        500..=599 => Color::Magenta,
        _ => Color::White,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            trace.status_code.to_string(),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" ({:.0?})", trace.duration)),
    ])];
    if let Some(body) = &trace.response_body {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Body:",
            Style::default().fg(Color::DarkGray),
        )));
        append_body_lines(
            &mut lines,
            body,
            trace.response_body_truncated,
            trace.response_body_binary,
            trace
                .response_headers
                .get("content-type")
                .map(String::as_str),
        );
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(no response body)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn headers_tab_lines(trace: &phantom_core::trace::HttpTrace) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "Request headers",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    let mut req_headers: Vec<(&String, &String)> = trace.request_headers.iter().collect();
    req_headers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in req_headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{key}: "), Style::default().fg(Color::Yellow)),
            Span::raw(truncate_str(value, 80)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Response headers",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    let mut resp_headers: Vec<(&String, &String)> = trace.response_headers.iter().collect();
    resp_headers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in resp_headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{key}: "), Style::default().fg(Color::Yellow)),
            Span::raw(truncate_str(value, 80)),
        ]));
    }
    lines
}

fn timing_tab_lines(trace: &phantom_core::trace::HttpTrace) -> Vec<Line<'static>> {
    let row = |label: &'static str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label}: "), Style::default().fg(Color::Yellow)),
            Span::raw(value),
        ])
    };
    let mut lines = vec![
        row("Timestamp", format_time(&trace.timestamp)),
        row("Duration", format!("{:.2?}", trace.duration)),
        row("Protocol", trace.protocol_version.clone()),
        row("Trace ID", trace.trace_id.to_string()),
        row("Span ID", trace.span_id.to_string()),
    ];
    if let Some(addr) = &trace.source_addr {
        lines.push(row("Source", addr.clone()));
    }
    if let Some(addr) = &trace.dest_addr {
        lines.push(row("Destination", addr.clone()));
    }
    lines
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
            Span::styled("[[/]]", Style::default().fg(Color::Yellow)),
            Span::raw("tab  "),
            Span::styled("[c/w]", Style::default().fg(Color::Yellow)),
            Span::raw("copy/save  "),
            Span::styled("[?]", Style::default().fg(Color::Yellow)),
            Span::raw("help"),
        ])
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let width = 62.min(area.width.saturating_sub(4));
    let height = 24.min(area.height.saturating_sub(4));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);

    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(Color::Yellow);
    let row = |key: &'static str, desc: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {key:<14}"), key_style),
            Span::raw(desc),
        ])
    };

    let lines = vec![
        Line::from(Span::styled("Keybindings", title_style)),
        row("q / Ctrl-C", "quit"),
        row("j/k, ↑/↓", "move selection (list) or scroll (detail)"),
        row("g/G, Home/End", "jump to top/bottom of the list"),
        row("Tab", "switch focus between list and detail pane"),
        row(
            "[ / ]",
            "switch detail tab (Request/Response/Headers/Timing)",
        ),
        row("/", "start filtering"),
        row("Esc", "clear filter / close this help"),
        row("c", "copy selected trace as a curl command"),
        row("w", "write selected trace to phantom-trace-<id>.json"),
        row("?", "toggle this help"),
        Line::from(""),
        Line::from(Span::styled("Filter syntax", title_style)),
        row("status:404", "exact status code"),
        row("status:4xx", "status class (1xx..5xx)"),
        row("status:>=500", "comparison: >=, <=, >, <"),
        row("method:post", "HTTP method (case-insensitive)"),
        row("host:example.com", "substring match on URL host"),
        row("path:/users", "substring match on URL path"),
        Line::from("  Multiple tokens are space-separated and ANDed together."),
        Line::from("  Anything else is a plain substring match on the full URL."),
        Line::from(""),
        Line::from(Span::styled(
            "[Esc] or [?] to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Help ");
    let help = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(help, popup);
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

fn append_body_lines(
    lines: &mut Vec<Line>,
    body: &[u8],
    truncated: bool,
    is_binary: bool,
    content_type: Option<&str>,
) {
    if is_binary {
        let ct = content_type.unwrap_or("application/octet-stream");
        lines.push(Line::from(Span::styled(
            format!("[binary body, {}, {ct}]", human_size(body.len())),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let text = String::from_utf8_lossy(body);
        // Try pretty-printing JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && let Ok(pretty) = serde_json::to_string_pretty(&json)
        {
            for line in pretty.lines().take(30) {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                )));
            }
        } else {
            for line in text.lines().take(30) {
                lines.push(Line::from(line.to_string()));
            }
        }
    }

    if truncated {
        lines.push(Line::from(Span::styled(
            format!(
                "[body truncated at {} — rerun with --max-body 0]",
                human_size(body.len())
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    }
}

/// Format a byte count as a human-readable size (e.g. "1.0 MB", "24.3 KB").
fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
