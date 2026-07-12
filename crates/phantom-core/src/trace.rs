use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Decodes a fixed-length lowercase/uppercase hex string into a byte array.
fn decode_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        bytes[i] = ((hi << 4) | lo) as u8;
    }
    Some(bytes)
}

/// Unique identifier for a trace (W3C Trace Context compatible, 128-bit).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(pub [u8; 16]);

impl TraceId {
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// Parses a 32-character hex string (the `Display` format) back into a `TraceId`.
    pub fn from_hex(s: &str) -> Option<Self> {
        decode_hex(s).map(Self)
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

    /// Parses a 16-character hex string (the `Display` format) back into a `SpanId`.
    pub fn from_hex(s: &str) -> Option<Self> {
        decode_hex(s).map(Self)
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

/// Error returned when parsing an unrecognized HTTP method string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown HTTP method: {0:?}")]
pub struct ParseMethodError(pub String);

impl FromStr for HttpMethod {
    type Err = ParseMethodError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "DELETE" => Ok(Self::Delete),
            "PATCH" => Ok(Self::Patch),
            "HEAD" => Ok(Self::Head),
            "OPTIONS" => Ok(Self::Options),
            "TRACE" => Ok(Self::Trace),
            "CONNECT" => Ok(Self::Connect),
            _ => Err(ParseMethodError(s.to_string())),
        }
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

    // -- Response --
    pub status_code: u16,
    pub response_headers: HashMap<String, String>,
    pub response_body: Option<Vec<u8>>,

    // -- Timing --
    pub timestamp: SystemTime,
    pub duration: Duration,

    // -- Metadata --
    pub source_addr: Option<String>,
    pub dest_addr: Option<String>,
    pub protocol_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_id_from_hex_round_trip() {
        let id = SpanId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        let hex = id.to_string();
        assert_eq!(hex, "0123456789abcdef");
        assert_eq!(SpanId::from_hex(&hex), Some(id));
    }

    #[test]
    fn test_trace_id_from_hex_round_trip() {
        let id = TraceId([0xff; 16]);
        let hex = id.to_string();
        assert_eq!(hex.len(), 32);
        assert_eq!(TraceId::from_hex(&hex), Some(id));
        // uppercase also accepted
        assert_eq!(
            TraceId::from_hex(&hex.to_uppercase()),
            Some(TraceId([0xff; 16]))
        );
    }

    #[test]
    fn test_from_hex_rejects_invalid() {
        assert_eq!(SpanId::from_hex(""), None);
        assert_eq!(SpanId::from_hex("0123456789abcde"), None); // too short
        assert_eq!(SpanId::from_hex("0123456789abcdef00"), None); // too long
        assert_eq!(SpanId::from_hex("0123456789abcdeg"), None); // non-hex char
        assert_eq!(TraceId::from_hex("0123456789abcdef"), None); // span-length for trace
    }

    #[test]
    fn test_http_method_from_str() {
        assert_eq!("GET".parse::<HttpMethod>().unwrap(), HttpMethod::Get);
        assert_eq!("get".parse::<HttpMethod>().unwrap(), HttpMethod::Get);
        assert_eq!("Post".parse::<HttpMethod>().unwrap(), HttpMethod::Post);
        assert_eq!("DELETE".parse::<HttpMethod>().unwrap(), HttpMethod::Delete);
        assert!("FETCH".parse::<HttpMethod>().is_err());
        assert!("".parse::<HttpMethod>().is_err());
    }
}
