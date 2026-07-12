use std::collections::HashMap;
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::trace::HttpTrace;

/// Controls how much of a trace is included when rendering a [`TraceView`].
///
/// Defaults include everything: unlimited bodies, no redaction.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Maximum body bytes to include; bodies longer than this are truncated
    /// at a UTF-8 character boundary and flagged. `None` = unlimited.
    pub max_body: Option<usize>,
    /// Omit request/response bodies entirely (original sizes still reported).
    pub headers_only: bool,
    /// Header names (lower-cased) whose values are replaced with `"[redacted]"`.
    pub redact_headers: Vec<String>,
}

impl RenderOptions {
    /// Headers redacted by default in agent-facing contexts (MCP tools).
    pub fn sensitive_headers() -> Vec<String> {
        [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}

/// Agent-friendly, fully serializable representation of an [`HttpTrace`].
///
/// This is the canonical JSON shape shared by the JSONL output stream, the
/// query CLI, and the MCP server. With default [`RenderOptions`] the emitted
/// fields are a superset of the historical JSONL schema (add-only:
/// `*_body_bytes` and `*_body_truncated`).
#[derive(Debug, Clone, Serialize)]
pub struct TraceView {
    /// Unix timestamp of the request in milliseconds.
    pub timestamp_ms: u64,
    /// Round-trip duration in milliseconds.
    pub duration_ms: u64,
    /// HTTP method ("GET", "POST", …).
    pub method: String,
    /// Full request URL.
    pub url: String,
    /// HTTP response status code.
    pub status_code: u16,
    /// Request headers (lower-cased keys).
    pub request_headers: HashMap<String, String>,
    /// Response headers (lower-cased keys).
    pub response_headers: HashMap<String, String>,
    /// Request body decoded as UTF-8 (replacement chars for non-UTF-8 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// Response body decoded as UTF-8 (replacement chars for non-UTF-8 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// Original request body size in bytes (present iff a body existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body_bytes: Option<u64>,
    /// Original response body size in bytes (present iff a body existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body_bytes: Option<u64>,
    /// True when `request_body` was truncated by `max_body`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub request_body_truncated: bool,
    /// True when `response_body` was truncated by `max_body`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub response_body_truncated: bool,
    /// Source socket address, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_addr: Option<String>,
    /// Destination socket address, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_addr: Option<String>,
    /// HTTP protocol version string (e.g. "HTTP/1.1").
    pub protocol_version: String,
    /// 128-bit W3C trace ID (hex).
    pub trace_id: String,
    /// 64-bit span ID (hex).
    pub span_id: String,
}

/// Decodes a body as lossy UTF-8, applying `headers_only`/`max_body` policy.
/// Returns `(rendered_body, original_size, truncated)`.
fn render_body(
    body: &Option<Vec<u8>>,
    opts: &RenderOptions,
) -> (Option<String>, Option<u64>, bool) {
    let Some(bytes) = body.as_ref() else {
        return (None, None, false);
    };
    let size = Some(bytes.len() as u64);
    if opts.headers_only {
        return (None, size, false);
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    let mut truncated = false;
    if let Some(max) = opts.max_body
        && text.len() > max
    {
        let mut cut = max;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        truncated = true;
    }
    (Some(text), size, truncated)
}

fn render_headers(headers: &HashMap<String, String>, redact: &[String]) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            if redact.iter().any(|r| r.eq_ignore_ascii_case(k)) {
                (k.clone(), "[redacted]".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

impl TraceView {
    pub fn render(trace: &HttpTrace, opts: &RenderOptions) -> Self {
        let timestamp_ms = trace
            .timestamp
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let (request_body, request_body_bytes, request_body_truncated) =
            render_body(&trace.request_body, opts);
        let (response_body, response_body_bytes, response_body_truncated) =
            render_body(&trace.response_body, opts);

        Self {
            timestamp_ms,
            duration_ms: trace.duration.as_millis() as u64,
            method: trace.method.to_string(),
            url: trace.url.clone(),
            status_code: trace.status_code,
            request_headers: render_headers(&trace.request_headers, &opts.redact_headers),
            response_headers: render_headers(&trace.response_headers, &opts.redact_headers),
            request_body,
            response_body,
            request_body_bytes,
            response_body_bytes,
            request_body_truncated,
            response_body_truncated,
            source_addr: trace.source_addr.clone(),
            dest_addr: trace.dest_addr.clone(),
            protocol_version: trace.protocol_version.clone(),
            trace_id: trace.trace_id.to_string(),
            span_id: trace.span_id.to_string(),
        }
    }
}

impl From<&HttpTrace> for TraceView {
    fn from(trace: &HttpTrace) -> Self {
        Self::render(trace, &RenderOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::trace::{HttpMethod, SpanId, TraceId};

    fn make_trace(request_body: Option<Vec<u8>>, response_body: Option<Vec<u8>>) -> HttpTrace {
        let mut request_headers = HashMap::new();
        request_headers.insert("authorization".to_string(), "Bearer secret".to_string());
        request_headers.insert("accept".to_string(), "application/json".to_string());
        HttpTrace {
            span_id: SpanId([1; 8]),
            trace_id: TraceId([2; 16]),
            parent_span_id: None,
            method: HttpMethod::Post,
            url: "http://example.com/api".to_string(),
            request_headers,
            request_body,
            status_code: 200,
            response_headers: HashMap::new(),
            response_body,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_millis(1500),
            duration: Duration::from_millis(42),
            source_addr: None,
            dest_addr: None,
            protocol_version: "HTTP/1.1".to_string(),
        }
    }

    #[test]
    fn test_render_default_keeps_full_body() {
        let t = make_trace(Some(b"hello".to_vec()), None);
        let v = TraceView::render(&t, &RenderOptions::default());
        assert_eq!(v.request_body.as_deref(), Some("hello"));
        assert_eq!(v.request_body_bytes, Some(5));
        assert!(!v.request_body_truncated);
        assert_eq!(v.response_body, None);
        assert_eq!(v.response_body_bytes, None);
        assert_eq!(v.timestamp_ms, 1500);
        assert_eq!(v.duration_ms, 42);
        assert_eq!(v.span_id, "0101010101010101");
    }

    #[test]
    fn test_render_max_body_truncates() {
        let t = make_trace(None, Some(b"0123456789".to_vec()));
        let opts = RenderOptions {
            max_body: Some(4),
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.response_body.as_deref(), Some("0123"));
        assert_eq!(v.response_body_bytes, Some(10));
        assert!(v.response_body_truncated);
    }

    #[test]
    fn test_render_truncation_respects_char_boundary() {
        // "あ" is 3 bytes in UTF-8; cutting at byte 4 must back up to byte 3.
        let t = make_trace(Some("ああ".as_bytes().to_vec()), None);
        let opts = RenderOptions {
            max_body: Some(4),
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_body.as_deref(), Some("あ"));
        assert_eq!(v.request_body_bytes, Some(6));
        assert!(v.request_body_truncated);
    }

    #[test]
    fn test_render_headers_only_omits_bodies_reports_sizes() {
        let t = make_trace(Some(b"req".to_vec()), Some(b"resp".to_vec()));
        let opts = RenderOptions {
            headers_only: true,
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_body, None);
        assert_eq!(v.response_body, None);
        assert_eq!(v.request_body_bytes, Some(3));
        assert_eq!(v.response_body_bytes, Some(4));
        assert!(!v.request_body_truncated);
    }

    #[test]
    fn test_render_no_truncation_when_body_fits() {
        let t = make_trace(Some(b"1234".to_vec()), None);
        let opts = RenderOptions {
            max_body: Some(4),
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_body.as_deref(), Some("1234"));
        assert!(!v.request_body_truncated);
        assert_eq!(v.request_body_bytes, Some(4));
    }

    #[test]
    fn test_render_max_body_zero_empties_body() {
        let t = make_trace(Some(b"data".to_vec()), None);
        let opts = RenderOptions {
            max_body: Some(0),
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_body.as_deref(), Some(""));
        assert!(v.request_body_truncated);
        assert_eq!(v.request_body_bytes, Some(4));
    }

    #[test]
    fn test_render_lossy_decodes_invalid_utf8() {
        let t = make_trace(Some(vec![0x68, 0x69, 0xff, 0xfe]), None);
        let v = TraceView::render(&t, &RenderOptions::default());
        let body = v.request_body.unwrap();
        assert!(body.starts_with("hi"));
        assert!(body.contains('\u{fffd}'));
        // Size reports the original bytes, not the decoded string length.
        assert_eq!(v.request_body_bytes, Some(4));
    }

    #[test]
    fn test_render_redacts_headers() {
        let t = make_trace(None, None);
        let opts = RenderOptions {
            redact_headers: RenderOptions::sensitive_headers(),
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_headers["authorization"], "[redacted]");
        assert_eq!(v.request_headers["accept"], "application/json");
    }

    #[test]
    fn test_render_redaction_is_case_insensitive() {
        let mut t = make_trace(None, None);
        t.response_headers
            .insert("Set-Cookie".to_string(), "sid=1".to_string());
        let opts = RenderOptions {
            redact_headers: vec!["AUTHORIZATION".to_string(), "set-cookie".to_string()],
            ..Default::default()
        };
        let v = TraceView::render(&t, &opts);
        assert_eq!(v.request_headers["authorization"], "[redacted]");
        assert_eq!(v.response_headers["Set-Cookie"], "[redacted]");
    }

    #[test]
    fn test_sensitive_headers_cover_credential_carriers() {
        let list = RenderOptions::sensitive_headers();
        for h in [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
        ] {
            assert!(list.contains(&h.to_string()), "missing {h}");
        }
    }

    #[test]
    fn test_serialized_shape_skips_absent_fields() {
        let t = make_trace(None, None);
        let json = serde_json::to_value(TraceView::from(&t)).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("request_body"));
        assert!(!obj.contains_key("request_body_bytes"));
        assert!(!obj.contains_key("request_body_truncated"));
        assert_eq!(obj["method"], "POST");
    }
}
