use std::str::FromStr;
use std::time::SystemTime;

use crate::trace::{HttpMethod, HttpTrace, TraceId};

/// Error returned when parsing a status-range expression.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid status range: {0:?} (expected e.g. \"404\", \"4xx\", or \"400-499\")")]
pub struct ParseStatusRangeError(pub String);

/// An inclusive range of HTTP status codes.
///
/// Parses from `"404"` (exact), `"4xx"` (class), or `"400-499"` (explicit range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusRange {
    pub min: u16,
    pub max: u16,
}

impl StatusRange {
    pub fn contains(&self, status: u16) -> bool {
        (self.min..=self.max).contains(&status)
    }
}

impl FromStr for StatusRange {
    type Err = ParseStatusRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseStatusRangeError(s.to_string());

        // "4xx" / "4XX" — status class
        if s.len() == 3 && s[1..].eq_ignore_ascii_case("xx") {
            let class = s[..1].parse::<u16>().map_err(|_| err())?;
            if !(1..=5).contains(&class) {
                return Err(err());
            }
            return Ok(Self {
                min: class * 100,
                max: class * 100 + 99,
            });
        }

        // "400-499" — explicit range
        if let Some((lo, hi)) = s.split_once('-') {
            let min = lo.parse::<u16>().map_err(|_| err())?;
            let max = hi.parse::<u16>().map_err(|_| err())?;
            if min > max {
                return Err(err());
            }
            return Ok(Self { min, max });
        }

        // "404" — exact code
        let code = s.parse::<u16>().map_err(|_| err())?;
        Ok(Self {
            min: code,
            max: code,
        })
    }
}

/// A filter over stored traces. All set fields must match (logical AND);
/// unset fields match everything.
#[derive(Debug, Clone, Default)]
pub struct TraceQuery {
    /// Match any of these methods; empty = all methods.
    pub methods: Vec<HttpMethod>,
    /// Match status codes within this inclusive range.
    pub status: Option<StatusRange>,
    /// Match URLs containing this substring (case-insensitive).
    pub url_contains: Option<String>,
    /// Only traces with `timestamp >= since` (inclusive).
    pub since: Option<SystemTime>,
    /// Only traces with `timestamp <= until` (inclusive).
    pub until: Option<SystemTime>,
    /// Restrict to spans of this trace ID.
    pub trace_id: Option<TraceId>,
    /// Maximum number of traces to return; 0 means the caller's default.
    pub limit: usize,
    /// Number of matching traces to skip (applied after filtering).
    pub offset: usize,
}

impl TraceQuery {
    /// Returns true when `trace` matches every set filter field.
    /// Pure predicate — storage layers scan and call this per trace.
    pub fn matches(&self, trace: &HttpTrace) -> bool {
        if !self.methods.is_empty() && !self.methods.contains(&trace.method) {
            return false;
        }
        if let Some(range) = &self.status
            && !range.contains(trace.status_code)
        {
            return false;
        }
        if let Some(pattern) = &self.url_contains
            && !trace.url.to_lowercase().contains(&pattern.to_lowercase())
        {
            return false;
        }
        if let Some(since) = self.since
            && trace.timestamp < since
        {
            return false;
        }
        if let Some(until) = self.until
            && trace.timestamp > until
        {
            return false;
        }
        if let Some(trace_id) = &self.trace_id
            && &trace.trace_id != trace_id
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::trace::SpanId;

    fn make_trace(method: HttpMethod, url: &str, status: u16, ts_secs: u64) -> HttpTrace {
        HttpTrace {
            span_id: SpanId([1; 8]),
            trace_id: TraceId([2; 16]),
            parent_span_id: None,
            method,
            url: url.to_string(),
            request_headers: HashMap::new(),
            request_body: None,
            status_code: status,
            response_headers: HashMap::new(),
            response_body: None,
            timestamp: UNIX_EPOCH + Duration::from_secs(ts_secs),
            duration: Duration::from_millis(10),
            source_addr: None,
            dest_addr: None,
            protocol_version: "HTTP/1.1".to_string(),
        }
    }

    #[test]
    fn test_status_range_parse_exact() {
        let r: StatusRange = "404".parse().unwrap();
        assert_eq!(r, StatusRange { min: 404, max: 404 });
        assert!(r.contains(404));
        assert!(!r.contains(403));
    }

    #[test]
    fn test_status_range_parse_class() {
        let r: StatusRange = "4xx".parse().unwrap();
        assert_eq!(r, StatusRange { min: 400, max: 499 });
        let r: StatusRange = "5XX".parse().unwrap();
        assert_eq!(r, StatusRange { min: 500, max: 599 });
        assert!("0xx".parse::<StatusRange>().is_err());
        assert!("6xx".parse::<StatusRange>().is_err());
    }

    #[test]
    fn test_status_range_parse_explicit() {
        let r: StatusRange = "400-499".parse().unwrap();
        assert_eq!(r, StatusRange { min: 400, max: 499 });
        assert!("499-400".parse::<StatusRange>().is_err());
        assert!("abc".parse::<StatusRange>().is_err());
        assert!("4x".parse::<StatusRange>().is_err());
        assert!("".parse::<StatusRange>().is_err());
    }

    #[test]
    fn test_query_default_matches_everything() {
        let q = TraceQuery::default();
        assert!(q.matches(&make_trace(HttpMethod::Get, "http://a/x", 200, 100)));
    }

    #[test]
    fn test_query_by_method() {
        let q = TraceQuery {
            methods: vec![HttpMethod::Post, HttpMethod::Put],
            ..Default::default()
        };
        assert!(q.matches(&make_trace(HttpMethod::Post, "http://a", 200, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "http://a", 200, 0)));
    }

    #[test]
    fn test_query_by_status() {
        let q = TraceQuery {
            status: Some("4xx".parse().unwrap()),
            ..Default::default()
        };
        assert!(q.matches(&make_trace(HttpMethod::Get, "http://a", 404, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "http://a", 200, 0)));
    }

    #[test]
    fn test_query_by_url_case_insensitive() {
        let q = TraceQuery {
            url_contains: Some("API/Users".to_string()),
            ..Default::default()
        };
        assert!(q.matches(&make_trace(HttpMethod::Get, "http://x/api/users/1", 200, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "http://x/health", 200, 0)));
    }

    #[test]
    fn test_query_time_range_inclusive() {
        let q = TraceQuery {
            since: Some(UNIX_EPOCH + Duration::from_secs(100)),
            until: Some(UNIX_EPOCH + Duration::from_secs(200)),
            ..Default::default()
        };
        assert!(!q.matches(&make_trace(HttpMethod::Get, "u", 200, 99)));
        assert!(q.matches(&make_trace(HttpMethod::Get, "u", 200, 100)));
        assert!(q.matches(&make_trace(HttpMethod::Get, "u", 200, 200)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "u", 200, 201)));
    }

    #[test]
    fn test_query_by_trace_id() {
        let q = TraceQuery {
            trace_id: Some(TraceId([2; 16])),
            ..Default::default()
        };
        assert!(q.matches(&make_trace(HttpMethod::Get, "u", 200, 0)));
        let q = TraceQuery {
            trace_id: Some(TraceId([9; 16])),
            ..Default::default()
        };
        assert!(!q.matches(&make_trace(HttpMethod::Get, "u", 200, 0)));
    }

    #[test]
    fn test_query_combined_filters() {
        let q = TraceQuery {
            methods: vec![HttpMethod::Get],
            status: Some("200".parse().unwrap()),
            url_contains: Some("/api".to_string()),
            ..Default::default()
        };
        assert!(q.matches(&make_trace(HttpMethod::Get, "http://x/api/a", 200, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Post, "http://x/api/a", 200, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "http://x/api/a", 500, 0)));
        assert!(!q.matches(&make_trace(HttpMethod::Get, "http://x/other", 200, 0)));
    }
}
