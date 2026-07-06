use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Unique identifier for a trace (W3C Trace Context compatible, 128-bit).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(pub [u8; 16]);

impl TraceId {
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Unique identifier for a span within a trace (64-bit).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanId(pub [u8; 8]);

impl SpanId {
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
    Trace,
    Connect,
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::Trace => "TRACE",
            Self::Connect => "CONNECT",
        };
        f.write_str(s)
    }
}

/// A complete HTTP request-response pair with timing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpTrace {
    /// Unique ID for this trace span.
    pub span_id: SpanId,
    /// W3C Trace Context trace ID (for distributed tracing).
    pub trace_id: TraceId,
    /// Parent span ID (`None` if root span).
    pub parent_span_id: Option<SpanId>,

    // -- Request --
    pub method: HttpMethod,
    pub url: String,
    pub request_headers: HashMap<String, String>,
    pub request_body: Option<Vec<u8>>,
    /// Original `Content-Encoding` of the request body, if it was transparently
    /// decoded for storage (e.g. `"gzip"`). `None` if the body was stored as-is.
    #[serde(default)]
    pub request_content_encoding: Option<String>,
    /// `true` if `request_body` was cut off at the configured size limit.
    #[serde(default)]
    pub request_body_truncated: bool,
    /// `true` if `request_body` looks like binary data (not valid UTF-8 text).
    #[serde(default)]
    pub request_body_binary: bool,

    // -- Response --
    pub status_code: u16,
    pub response_headers: HashMap<String, String>,
    pub response_body: Option<Vec<u8>>,
    /// Original `Content-Encoding` of the response body, if it was transparently
    /// decoded for storage (e.g. `"gzip"`). `None` if the body was stored as-is.
    #[serde(default)]
    pub response_content_encoding: Option<String>,
    /// `true` if `response_body` was cut off at the configured size limit.
    #[serde(default)]
    pub response_body_truncated: bool,
    /// `true` if `response_body` looks like binary data (not valid UTF-8 text).
    #[serde(default)]
    pub response_body_binary: bool,

    // -- Timing --
    pub timestamp: SystemTime,
    pub duration: Duration,

    // -- Metadata --
    pub source_addr: Option<String>,
    pub dest_addr: Option<String>,
    pub protocol_version: String,
}
