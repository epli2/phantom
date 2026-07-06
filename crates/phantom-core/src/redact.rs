//! Redact sensitive header values and JSON body fields on a captured
//! [`HttpTrace`] before it is stored, displayed, or exported anywhere.
//!
//! Redaction is opt-in (see `--redact` / `--redact-header` /
//! `--redact-body-field` in the CLI) and, when enabled, is applied exactly
//! once — immediately after a trace is built, before it reaches the trace
//! channel — so storage, TUI, and JSONL output all see the same redacted
//! copy.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::trace::HttpTrace;

/// Placeholder written in place of a redacted value. Deliberately carries no
/// length or content information about the original value.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Which header names and JSON body field names to redact. Names are
/// matched case-insensitively; store them lower-cased.
#[derive(Debug, Clone, Default)]
pub struct RedactionConfig {
    header_names: HashSet<String>,
    body_field_names: HashSet<String>,
}

impl RedactionConfig {
    /// Build a config from raw (possibly mixed-case) header and body field
    /// names, e.g. as collected from repeated `--redact-header`/
    /// `--redact-body-field` CLI flags.
    pub fn new<H, B>(header_names: H, body_field_names: B) -> Self
    where
        H: IntoIterator<Item = String>,
        B: IntoIterator<Item = String>,
    {
        Self {
            header_names: header_names
                .into_iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            body_field_names: body_field_names
                .into_iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
        }
    }

    /// `true` if this config redacts nothing (the default, off state).
    pub fn is_empty(&self) -> bool {
        self.header_names.is_empty() && self.body_field_names.is_empty()
    }

    /// Default header names redacted by plain `--redact`.
    pub fn default_header_names() -> Vec<String> {
        [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "x-api-key",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// Default JSON body field names redacted by plain `--redact`.
    pub fn default_body_field_names() -> Vec<String> {
        [
            "password",
            "token",
            "access_token",
            "refresh_token",
            "client_secret",
            "api_key",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }
}

/// Apply redaction to `trace` in place. No-op if `config` is empty.
pub fn redact_trace(trace: &mut HttpTrace, config: &RedactionConfig) {
    if config.is_empty() {
        return;
    }
    redact_headers(&mut trace.request_headers, config);
    redact_headers(&mut trace.response_headers, config);
    if let Some(body) = trace.request_body.as_mut() {
        redact_json_body(body, config);
    }
    if let Some(body) = trace.response_body.as_mut() {
        redact_json_body(body, config);
    }
}

fn redact_headers(headers: &mut HashMap<String, String>, config: &RedactionConfig) {
    if config.header_names.is_empty() {
        return;
    }
    for (key, value) in headers.iter_mut() {
        if config.header_names.contains(&key.to_ascii_lowercase()) {
            *value = REDACTED_PLACEHOLDER.to_string();
        }
    }
}

/// Redact matching keys in a JSON body, leaving non-JSON (or unparseable)
/// bodies untouched. Re-serializes the body on change, so exact original
/// whitespace/key order is not preserved — only the redacted values matter.
fn redact_json_body(body: &mut Vec<u8>, config: &RedactionConfig) {
    if config.body_field_names.is_empty() {
        return;
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return;
    };
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if redact_json_value(&mut value, config)
        && let Ok(serialized) = serde_json::to_vec(&value)
    {
        *body = serialized;
    }
}

fn redact_json_value(value: &mut Value, config: &RedactionConfig) -> bool {
    let mut changed = false;
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if config.body_field_names.contains(&key.to_ascii_lowercase()) {
                    *v = Value::String(REDACTED_PLACEHOLDER.to_string());
                    changed = true;
                } else {
                    changed |= redact_json_value(v, config);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                changed |= redact_json_value(v, config);
            }
        }
        _ => {}
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{HttpMethod, SpanId, TraceId};
    use std::time::{Duration, SystemTime};

    fn make_trace(
        request_headers: HashMap<String, String>,
        request_body: Option<&str>,
    ) -> HttpTrace {
        HttpTrace {
            span_id: SpanId([0; 8]),
            trace_id: TraceId([0; 16]),
            parent_span_id: None,
            method: HttpMethod::Post,
            url: "https://api.example.com/login".to_string(),
            request_headers,
            request_body: request_body.map(|s| s.as_bytes().to_vec()),
            request_content_encoding: None,
            request_body_truncated: false,
            request_body_binary: false,
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
    fn test_empty_config_is_noop() {
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), "Bearer secret".to_string());
        let mut trace = make_trace(headers, None);
        redact_trace(&mut trace, &RedactionConfig::default());
        assert_eq!(
            trace.request_headers.get("authorization"),
            Some(&"Bearer secret".to_string())
        );
    }

    #[test]
    fn test_redacts_configured_header() {
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), "Bearer secret".to_string());
        headers.insert("accept".to_string(), "application/json".to_string());
        let mut trace = make_trace(headers, None);
        let config = RedactionConfig::new(vec!["authorization".to_string()], vec![]);
        redact_trace(&mut trace, &config);
        assert_eq!(
            trace.request_headers.get("authorization"),
            Some(&REDACTED_PLACEHOLDER.to_string())
        );
        assert_eq!(
            trace.request_headers.get("accept"),
            Some(&"application/json".to_string())
        );
    }

    #[test]
    fn test_header_match_is_case_insensitive() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        let mut trace = make_trace(headers, None);
        let config = RedactionConfig::new(vec!["AUTHORIZATION".to_string()], vec![]);
        redact_trace(&mut trace, &config);
        assert_eq!(
            trace.request_headers.get("Authorization"),
            Some(&REDACTED_PLACEHOLDER.to_string())
        );
    }

    #[test]
    fn test_redacts_nested_json_body_field() {
        let body = r#"{"username":"alice","credentials":{"password":"hunter2"}}"#;
        let mut trace = make_trace(HashMap::new(), Some(body));
        let config = RedactionConfig::new(vec![], vec!["password".to_string()]);
        redact_trace(&mut trace, &config);
        let redacted: Value = serde_json::from_slice(trace.request_body.as_ref().unwrap()).unwrap();
        assert_eq!(redacted["username"], "alice");
        assert_eq!(redacted["credentials"]["password"], REDACTED_PLACEHOLDER);
    }

    #[test]
    fn test_redacts_json_field_in_array() {
        let body = r#"[{"token":"abc"},{"token":"def"}]"#;
        let mut trace = make_trace(HashMap::new(), Some(body));
        let config = RedactionConfig::new(vec![], vec!["token".to_string()]);
        redact_trace(&mut trace, &config);
        let redacted: Value = serde_json::from_slice(trace.request_body.as_ref().unwrap()).unwrap();
        assert_eq!(redacted[0]["token"], REDACTED_PLACEHOLDER);
        assert_eq!(redacted[1]["token"], REDACTED_PLACEHOLDER);
    }

    #[test]
    fn test_non_json_body_untouched() {
        let body = "password=hunter2&username=alice";
        let mut trace = make_trace(HashMap::new(), Some(body));
        let config = RedactionConfig::new(vec![], vec!["password".to_string()]);
        redact_trace(&mut trace, &config);
        assert_eq!(
            trace.request_body.as_deref(),
            Some(body.as_bytes()),
            "non-JSON bodies must be left untouched"
        );
    }

    #[test]
    fn test_unconfigured_body_field_untouched() {
        let body = r#"{"password":"hunter2"}"#;
        let mut trace = make_trace(HashMap::new(), Some(body));
        // Only headers configured, no body fields.
        let config = RedactionConfig::new(vec!["authorization".to_string()], vec![]);
        redact_trace(&mut trace, &config);
        let value: Value = serde_json::from_slice(trace.request_body.as_ref().unwrap()).unwrap();
        assert_eq!(value["password"], "hunter2");
    }
}
