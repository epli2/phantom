//! Convert a captured [`HttpTrace`] into forms useful outside the TUI: a
//! runnable `curl` command (`c` key) and a standalone JSON file (`w` key).
//!
//! This lives in `phantom-tui` for now because it's only consumed here; the
//! roadmap (P2-1) moves it into a dedicated `phantom-export` crate once HAR/
//! JSONL export need to share the same conversion logic.

use phantom_core::trace::HttpTrace;

/// Headers curl sets/derives itself; including stale captured values for
/// these would conflict with or duplicate what curl actually sends.
const SKIP_HEADERS: [&str; 3] = ["content-length", "host", "connection"];

/// Single-quote a value for safe inclusion in a POSIX shell command line:
/// close the quote, escape a literal `'`, reopen the quote.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Render `trace` as a `curl` command that reproduces the captured request.
/// Best-effort: binary request bodies are omitted (noted in a comment) since
/// they can't round-trip through a shell-quoted `--data-raw` argument.
pub fn trace_to_curl(trace: &HttpTrace) -> String {
    let mut cmd = format!("curl -X {} {}", trace.method, shell_quote(&trace.url));

    let mut headers: Vec<(&String, &String)> = trace.request_headers.iter().collect();
    headers.sort_by(|a, b| a.0.cmp(b.0));
    for (key, value) in headers {
        if SKIP_HEADERS.contains(&key.to_ascii_lowercase().as_str()) {
            continue;
        }
        cmd.push_str(" \\\n  -H ");
        cmd.push_str(&shell_quote(&format!("{key}: {value}")));
    }

    match &trace.request_body {
        Some(_) if trace.request_body_binary => {
            cmd.push_str("\\\n  # request body omitted: binary data");
        }
        Some(body) => {
            let text = String::from_utf8_lossy(body);
            cmd.push_str(" \\\n  --data-raw ");
            cmd.push_str(&shell_quote(&text));
        }
        None => {}
    }

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn make_trace(
        method: phantom_core::trace::HttpMethod,
        url: &str,
        request_headers: HashMap<String, String>,
        request_body: Option<Vec<u8>>,
        request_body_binary: bool,
    ) -> HttpTrace {
        HttpTrace {
            span_id: phantom_core::trace::SpanId([0; 8]),
            trace_id: phantom_core::trace::TraceId([0; 16]),
            parent_span_id: None,
            method,
            url: url.to_string(),
            request_headers,
            request_body,
            request_content_encoding: None,
            request_body_truncated: false,
            request_body_binary,
            status_code: 200,
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

    #[test]
    fn test_basic_get_command() {
        let t = make_trace(
            phantom_core::trace::HttpMethod::Get,
            "https://api.example.com/users",
            HashMap::new(),
            None,
            false,
        );
        let cmd = trace_to_curl(&t);
        assert_eq!(cmd, "curl -X GET 'https://api.example.com/users'");
    }

    #[test]
    fn test_includes_headers_but_skips_hop_by_hop() {
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), "Bearer x".to_string());
        headers.insert("host".to_string(), "api.example.com".to_string());
        headers.insert("content-length".to_string(), "3".to_string());
        let t = make_trace(
            phantom_core::trace::HttpMethod::Get,
            "https://api.example.com/users",
            headers,
            None,
            false,
        );
        let cmd = trace_to_curl(&t);
        assert!(cmd.contains("-H 'authorization: Bearer x'"));
        assert!(!cmd.contains("host:"));
        assert!(!cmd.contains("content-length:"));
    }

    #[test]
    fn test_includes_text_body() {
        let t = make_trace(
            phantom_core::trace::HttpMethod::Post,
            "https://api.example.com/users",
            HashMap::new(),
            Some(br#"{"name":"Alice"}"#.to_vec()),
            false,
        );
        let cmd = trace_to_curl(&t);
        assert!(cmd.contains("--data-raw"));
        assert!(cmd.contains(r#"{"name":"Alice"}"#));
    }

    #[test]
    fn test_binary_body_is_omitted_not_corrupted() {
        let t = make_trace(
            phantom_core::trace::HttpMethod::Post,
            "https://api.example.com/upload",
            HashMap::new(),
            Some(vec![0, 1, 2, 255]),
            true,
        );
        let cmd = trace_to_curl(&t);
        assert!(!cmd.contains("--data-raw"));
        assert!(cmd.contains("binary data"));
    }

    #[test]
    fn test_shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_header_value_with_quote_is_escaped() {
        let mut headers = HashMap::new();
        headers.insert("x-note".to_string(), "it's here".to_string());
        let t = make_trace(
            phantom_core::trace::HttpMethod::Get,
            "https://x/y",
            headers,
            None,
            false,
        );
        let cmd = trace_to_curl(&t);
        assert!(cmd.contains(r"x-note: it'\''s here"));
    }
}
