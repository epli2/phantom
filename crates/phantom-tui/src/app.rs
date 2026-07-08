use phantom_core::trace::HttpTrace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    TraceList,
    TraceDetail,
}

/// Which section of the detail pane is currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Request,
    Response,
    Headers,
    Timing,
}

impl DetailTab {
    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Request => "Request",
            DetailTab::Response => "Response",
            DetailTab::Headers => "Headers",
            DetailTab::Timing => "Timing",
        }
    }

    pub fn next(self) -> Self {
        match self {
            DetailTab::Request => DetailTab::Response,
            DetailTab::Response => DetailTab::Headers,
            DetailTab::Headers => DetailTab::Timing,
            DetailTab::Timing => DetailTab::Request,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            DetailTab::Request => DetailTab::Timing,
            DetailTab::Response => DetailTab::Request,
            DetailTab::Headers => DetailTab::Response,
            DetailTab::Timing => DetailTab::Headers,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Structured filter query language
//
// Space-separated tokens, ANDed together. Recognized `key:value` prefixes:
//   status:404        exact status code
//   status:4xx         status class (1xx..5xx)
//   status:>=500       comparison (>=, <=, >, <)
//   method:post         HTTP method, case-insensitive
//   host:api.example.com   substring match against the URL's host
//   path:/users         substring match against the URL's path
// Any other token (or a key:value token whose value fails to parse) falls
// back to a plain case-insensitive substring match against the full URL.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusMatch {
    Exact(u16),
    /// First digit of the status code, e.g. `4` for the 4xx class.
    Class(u16),
    Ge(u16),
    Le(u16),
    Gt(u16),
    Lt(u16),
}

impl StatusMatch {
    fn matches(&self, status: u16) -> bool {
        match self {
            StatusMatch::Exact(s) => status == *s,
            StatusMatch::Class(c) => status / 100 == *c,
            StatusMatch::Ge(s) => status >= *s,
            StatusMatch::Le(s) => status <= *s,
            StatusMatch::Gt(s) => status > *s,
            StatusMatch::Lt(s) => status < *s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FilterToken {
    Status(StatusMatch),
    Method(String),
    Host(String),
    Path(String),
    /// Plain substring match against the full URL (lower-cased).
    Text(String),
}

/// A parsed filter query: a list of tokens that must ALL match (AND).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterExpr {
    tokens: Vec<FilterToken>,
}

impl FilterExpr {
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn matches(&self, trace: &HttpTrace) -> bool {
        self.tokens.iter().all(|t| token_matches(t, trace))
    }
}

fn token_matches(token: &FilterToken, trace: &HttpTrace) -> bool {
    match token {
        FilterToken::Status(m) => m.matches(trace.status_code),
        FilterToken::Method(m) => trace.method.to_string().eq_ignore_ascii_case(m),
        FilterToken::Host(h) => extract_host(&trace.url).to_lowercase().contains(h),
        FilterToken::Path(p) => extract_path(&trace.url).to_lowercase().contains(p),
        FilterToken::Text(s) => trace.url.to_lowercase().contains(s),
    }
}

/// Extract the host (no port) from a full URL. Best-effort string parsing —
/// good enough for filtering, not a general-purpose URL parser.
fn extract_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host_port = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip a userinfo@ prefix and a :port suffix, but keep IPv6 [::1] intact.
    let host_port = host_port.rsplit('@').next().unwrap_or(host_port);
    if host_port.starts_with('[') {
        host_port
            .split_once(']')
            .map_or(host_port, |(h, _)| &h[1..])
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    }
}

/// Extract the path (including leading `/`) from a full URL.
fn extract_path(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx..],
        None => "/",
    }
}

fn parse_status_value(v: &str) -> Option<StatusMatch> {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix(">=") {
        return rest.parse().ok().map(StatusMatch::Ge);
    }
    if let Some(rest) = v.strip_prefix("<=") {
        return rest.parse().ok().map(StatusMatch::Le);
    }
    if let Some(rest) = v.strip_prefix('>') {
        return rest.parse().ok().map(StatusMatch::Gt);
    }
    if let Some(rest) = v.strip_prefix('<') {
        return rest.parse().ok().map(StatusMatch::Lt);
    }
    let lower = v.to_ascii_lowercase();
    if lower.len() == 3 && lower.ends_with("xx") {
        if let Some(d) = lower.as_bytes()[0].checked_sub(b'0')
            && d <= 9
        {
            return Some(StatusMatch::Class(d as u16));
        }
        return None;
    }
    v.parse().ok().map(StatusMatch::Exact)
}

/// Parse a filter query string into a [`FilterExpr`]. Never fails — anything
/// that doesn't parse as a recognized `key:value` falls back to a plain
/// substring token.
pub fn parse_filter(input: &str) -> FilterExpr {
    let tokens = input
        .split_whitespace()
        .map(|word| {
            let lower_word = word.to_lowercase();
            if let Some(v) = word.strip_prefix("status:") {
                match parse_status_value(v) {
                    Some(m) => FilterToken::Status(m),
                    None => FilterToken::Text(lower_word),
                }
            } else if let Some(v) = word.strip_prefix("method:") {
                if v.is_empty() {
                    FilterToken::Text(lower_word)
                } else {
                    FilterToken::Method(v.to_string())
                }
            } else if let Some(v) = word.strip_prefix("host:") {
                if v.is_empty() {
                    FilterToken::Text(lower_word)
                } else {
                    FilterToken::Host(v.to_lowercase())
                }
            } else if let Some(v) = word.strip_prefix("path:") {
                if v.is_empty() {
                    FilterToken::Text(lower_word)
                } else {
                    FilterToken::Path(v.to_lowercase())
                }
            } else {
                FilterToken::Text(lower_word)
            }
        })
        .collect();
    FilterExpr { tokens }
}

// ─────────────────────────────────────────────────────────────────────────────
// App state
// ─────────────────────────────────────────────────────────────────────────────

pub struct App {
    pub traces: Vec<HttpTrace>,
    pub selected_index: usize,
    pub filter: String,
    pub filter_active: bool,
    pub active_pane: Pane,
    pub should_quit: bool,
    pub trace_count: u64,
    pub backend_name: String,
    pub help_visible: bool,
    pub detail_tab: DetailTab,
    pub detail_scroll: u16,
    /// Transient feedback for the last `c`/`w` action (e.g. "Copied curl
    /// command", "Clipboard unavailable — printed to stderr"). Cleared on
    /// the next keypress.
    pub status_message: Option<String>,
}

impl App {
    pub fn new(backend_name: &str) -> Self {
        Self {
            traces: Vec::new(),
            selected_index: 0,
            filter: String::new(),
            filter_active: false,
            active_pane: Pane::TraceList,
            should_quit: false,
            trace_count: 0,
            backend_name: backend_name.to_string(),
            help_visible: false,
            detail_tab: DetailTab::Request,
            detail_scroll: 0,
            status_message: None,
        }
    }

    pub fn filtered_traces(&self) -> Vec<&HttpTrace> {
        let expr = parse_filter(&self.filter);
        if expr.is_empty() {
            self.traces.iter().collect()
        } else {
            self.traces.iter().filter(|t| expr.matches(t)).collect()
        }
    }

    pub fn selected_trace(&self) -> Option<&HttpTrace> {
        let filtered = self.filtered_traces();
        filtered.get(self.selected_index).copied()
    }

    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
        self.detail_scroll = 0;
    }

    pub fn move_down(&mut self) {
        let max = self.filtered_traces().len().saturating_sub(1);
        if self.selected_index < max {
            self.selected_index += 1;
        }
        self.detail_scroll = 0;
    }

    pub fn jump_top(&mut self) {
        self.selected_index = 0;
        self.detail_scroll = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.selected_index = self.filtered_traces().len().saturating_sub(1);
        self.detail_scroll = 0;
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            Pane::TraceList => Pane::TraceDetail,
            Pane::TraceDetail => Pane::TraceList,
        };
    }

    pub fn activate_filter(&mut self) {
        self.filter_active = true;
    }

    pub fn deactivate_filter(&mut self) {
        self.filter_active = false;
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_active = false;
        self.selected_index = 0;
        self.detail_scroll = 0;
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected_index = 0;
        self.detail_scroll = 0;
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.selected_index = 0;
        self.detail_scroll = 0;
    }

    pub fn add_trace(&mut self, trace: HttpTrace) {
        self.traces.insert(0, trace);
        self.trace_count += 1;
        // Keep selection stable when new traces arrive
        if !self.filter_active && self.selected_index > 0 {
            self.selected_index += 1;
        }
    }

    pub fn toggle_help(&mut self) {
        self.help_visible = !self.help_visible;
    }

    pub fn close_help(&mut self) {
        self.help_visible = false;
    }

    pub fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    pub fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    pub fn next_detail_tab(&mut self) {
        self.detail_tab = self.detail_tab.next();
        self.detail_scroll = 0;
    }

    pub fn prev_detail_tab(&mut self) {
        self.detail_tab = self.detail_tab.prev();
        self.detail_scroll = 0;
    }

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    pub fn clear_status_message(&mut self) {
        self.status_message = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn make_trace(method: phantom_core::trace::HttpMethod, url: &str, status: u16) -> HttpTrace {
        HttpTrace {
            span_id: phantom_core::trace::SpanId([0; 8]),
            trace_id: phantom_core::trace::TraceId([0; 16]),
            parent_span_id: None,
            method,
            url: url.to_string(),
            request_headers: HashMap::new(),
            request_body: None,
            request_content_encoding: None,
            request_body_truncated: false,
            request_body_binary: false,
            status_code: status,
            response_headers: HashMap::new(),
            response_body: None,
            response_content_encoding: None,
            response_body_truncated: false,
            response_body_binary: false,
            timestamp: SystemTime::now(),
            duration: Duration::from_millis(1),
            source_addr: None,
            dest_addr: None,
            protocol_version: "HTTP/1.1".to_string(),
        }
    }

    use phantom_core::trace::HttpMethod;

    #[test]
    fn test_empty_filter_matches_everything() {
        let expr = parse_filter("");
        assert!(expr.is_empty());
        let t = make_trace(HttpMethod::Get, "https://api.example.com/users", 200);
        assert!(expr.matches(&t));
    }

    #[test]
    fn test_plain_text_matches_url_substring() {
        let expr = parse_filter("users");
        assert!(expr.matches(&make_trace(
            HttpMethod::Get,
            "https://api.example.com/users",
            200
        )));
        assert!(!expr.matches(&make_trace(
            HttpMethod::Get,
            "https://api.example.com/orders",
            200
        )));
    }

    #[test]
    fn test_status_exact_match() {
        let expr = parse_filter("status:404");
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 404)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
    }

    #[test]
    fn test_status_class_match() {
        let expr = parse_filter("status:4xx");
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 404)));
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 499)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 500)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
    }

    #[test]
    fn test_status_ge_match() {
        let expr = parse_filter("status:>=500");
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 500)));
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 503)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 499)));
    }

    #[test]
    fn test_status_lt_match() {
        let expr = parse_filter("status:<300");
        assert!(expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 300)));
    }

    #[test]
    fn test_method_match_case_insensitive() {
        let expr = parse_filter("method:post");
        assert!(expr.matches(&make_trace(HttpMethod::Post, "https://x/y", 200)));
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));

        let expr_upper = parse_filter("method:GET");
        assert!(expr_upper.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
    }

    #[test]
    fn test_host_match() {
        let expr = parse_filter("host:api.example.com");
        assert!(expr.matches(&make_trace(
            HttpMethod::Get,
            "https://api.example.com/users",
            200
        )));
        assert!(!expr.matches(&make_trace(
            HttpMethod::Get,
            "https://other.example.com/users",
            200
        )));
    }

    #[test]
    fn test_path_match() {
        let expr = parse_filter("path:/users");
        assert!(expr.matches(&make_trace(
            HttpMethod::Get,
            "https://api.example.com/users/42",
            200
        )));
        assert!(!expr.matches(&make_trace(
            HttpMethod::Get,
            "https://api.example.com/orders",
            200
        )));
    }

    #[test]
    fn test_multiple_tokens_are_anded() {
        let expr = parse_filter("status:5xx method:post");
        let matching = make_trace(HttpMethod::Post, "https://x/y", 503);
        let wrong_method = make_trace(HttpMethod::Get, "https://x/y", 503);
        let wrong_status = make_trace(HttpMethod::Post, "https://x/y", 200);
        assert!(expr.matches(&matching));
        assert!(!expr.matches(&wrong_method));
        assert!(!expr.matches(&wrong_status));
    }

    #[test]
    fn test_invalid_status_value_falls_back_to_text() {
        let expr = parse_filter("status:abc");
        // Falls back to a literal substring match on "status:abc", which
        // won't appear in a normal URL — so nothing should match.
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
        // But it must not panic, and should behave as a plain substring
        // filter if the literal text does appear.
        let expr2 = parse_filter("status:abc");
        assert!(expr2.matches(&make_trace(HttpMethod::Get, "https://x/status:abc", 200)));
    }

    #[test]
    fn test_empty_value_keyword_falls_back_to_text() {
        let expr = parse_filter("host:");
        // "host:" itself is treated as a literal substring (won't match a
        // normal URL) rather than panicking on an empty host name.
        assert!(!expr.matches(&make_trace(HttpMethod::Get, "https://x/y", 200)));
    }

    #[test]
    fn test_extract_host_and_path_helpers() {
        assert_eq!(
            extract_host("https://api.example.com:8080/users?x=1"),
            "api.example.com"
        );
        assert_eq!(
            extract_path("https://api.example.com:8080/users?x=1"),
            "/users?x=1"
        );
    }

    #[test]
    fn test_detail_tab_cycle() {
        let mut tab = DetailTab::Request;
        tab = tab.next();
        assert_eq!(tab, DetailTab::Response);
        tab = tab.next();
        assert_eq!(tab, DetailTab::Headers);
        tab = tab.next();
        assert_eq!(tab, DetailTab::Timing);
        tab = tab.next();
        assert_eq!(tab, DetailTab::Request);
        tab = tab.prev();
        assert_eq!(tab, DetailTab::Timing);
    }

    #[test]
    fn test_scroll_and_selection_reset_detail_scroll() {
        let mut app = App::new("test");
        app.traces
            .push(make_trace(HttpMethod::Get, "https://x/1", 200));
        app.traces
            .push(make_trace(HttpMethod::Get, "https://x/2", 200));
        app.scroll_detail_down();
        app.scroll_detail_down();
        assert_eq!(app.detail_scroll, 2);
        app.move_down();
        assert_eq!(app.detail_scroll, 0, "moving selection resets scroll");
    }

    #[test]
    fn test_help_toggle() {
        let mut app = App::new("test");
        assert!(!app.help_visible);
        app.toggle_help();
        assert!(app.help_visible);
        app.close_help();
        assert!(!app.help_visible);
    }
}
