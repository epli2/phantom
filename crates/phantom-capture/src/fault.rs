/// A single fault injection rule, evaluated per request.
#[derive(Clone, Debug)]
pub enum FaultRule {
    /// Inject artificial latency before forwarding the request.
    Delay {
        min_ms: u64,
        max_ms: u64,
        /// If Some, only applies when the URL contains this substring.
        url_pattern: Option<String>,
    },
    /// Return a synthetic HTTP error response instead of forwarding.
    Error {
        status_code: u16,
        /// Probability 0.0–1.0 (1.0 = always inject).
        probability: f64,
        /// If Some, only applies when the URL contains this substring.
        url_pattern: Option<String>,
    },
}

impl FaultRule {
    /// Returns true if this rule applies to the given URL.
    pub fn matches_url(&self, url: &str) -> bool {
        let pattern = match self {
            FaultRule::Delay { url_pattern, .. } => url_pattern,
            FaultRule::Error { url_pattern, .. } => url_pattern,
        };
        pattern.as_ref().map(|p| url.contains(p.as_str())).unwrap_or(true)
    }
}

/// A collection of fault rules applied in order to each proxied request.
#[derive(Clone, Debug, Default)]
pub struct FaultConfig {
    pub rules: Vec<FaultRule>,
}

// ─────────────────────────────────────────────────────────────────────────────
// CLI spec parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a fault specification string into a `FaultRule`.
///
/// Formats:
///   delay:100ms                 fixed 100ms delay on all requests
///   delay:100ms-500ms           random delay in 100–500ms range
///   delay:100ms:/api            delay only URLs containing "/api"
///   delay:100ms-500ms:/api      range delay with URL filter
///   error:503                   always return HTTP 503
///   error:503:0.5               return HTTP 503 with 50% probability
///   error:503:/api              always return 503 for URLs containing "/api"
///   error:503:0.5:/api          probability + URL filter
pub fn parse_fault_spec(s: &str) -> Result<FaultRule, String> {
    let (kind, rest) = s
        .split_once(':')
        .ok_or_else(|| format!("invalid fault spec {s:?}: expected 'delay:…' or 'error:…'"))?;
    match kind {
        "delay" => parse_delay(rest),
        "error" => parse_error(rest),
        _ => Err(format!(
            "unknown fault type {kind:?} in {s:?}; expected 'delay' or 'error'"
        )),
    }
}

fn parse_delay(rest: &str) -> Result<FaultRule, String> {
    let (timing, url_pattern) = split_url_suffix(rest);
    if let Some(dash) = timing.find('-') {
        let min_ms = parse_ms(&timing[..dash])?;
        let max_ms = parse_ms(&timing[dash + 1..])?;
        if min_ms > max_ms {
            return Err(format!(
                "delay range min ({min_ms}ms) must not exceed max ({max_ms}ms)"
            ));
        }
        Ok(FaultRule::Delay {
            min_ms,
            max_ms,
            url_pattern,
        })
    } else {
        let ms = parse_ms(timing)?;
        Ok(FaultRule::Delay {
            min_ms: ms,
            max_ms: ms,
            url_pattern,
        })
    }
}

fn parse_error(rest: &str) -> Result<FaultRule, String> {
    // Optional URL pattern is the last colon-segment starting with '/'.
    let (url_pattern, non_url) = if let Some(pos) = rest.rfind(':') {
        if rest[pos + 1..].starts_with('/') {
            (Some(rest[pos + 1..].to_string()), &rest[..pos])
        } else {
            (None, rest)
        }
    } else {
        (None, rest)
    };

    let parts: Vec<&str> = non_url.splitn(2, ':').collect();
    let status_code: u16 = parts[0]
        .parse()
        .map_err(|_| format!("invalid HTTP status code {:?}", parts[0]))?;
    if !(100..=599).contains(&status_code) {
        return Err(format!(
            "status code {status_code} is out of range 100–599"
        ));
    }
    let probability: f64 = if parts.len() == 2 {
        parts[1]
            .parse()
            .map_err(|_| format!("invalid probability {:?}; expected a float like 0.5", parts[1]))?
    } else {
        1.0
    };
    if !(0.0..=1.0).contains(&probability) {
        return Err(format!(
            "probability {probability} is out of range 0.0–1.0"
        ));
    }
    Ok(FaultRule::Error {
        status_code,
        probability,
        url_pattern,
    })
}

/// Split a trailing URL pattern (`:/<path>`) from the rest of a spec segment.
fn split_url_suffix(s: &str) -> (&str, Option<String>) {
    if let Some(pos) = s.rfind(':') {
        if s[pos + 1..].starts_with('/') {
            return (&s[..pos], Some(s[pos + 1..].to_string()));
        }
    }
    (s, None)
}

fn parse_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        n.parse::<u64>()
            .map_err(|_| format!("invalid duration {s:?}; expected e.g. '100ms'"))
    } else if let Some(n) = s.strip_suffix('s') {
        n.parse::<u64>()
            .map(|v| v * 1000)
            .map_err(|_| format!("invalid duration {s:?}; expected e.g. '2s'"))
    } else {
        s.parse::<u64>()
            .map_err(|_| format!("invalid duration {s:?}; expected e.g. '100ms' or '2s'"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fixed_delay() {
        let r = parse_fault_spec("delay:200ms").unwrap();
        assert!(matches!(r, FaultRule::Delay { min_ms: 200, max_ms: 200, url_pattern: None }));
    }

    #[test]
    fn parse_range_delay() {
        let r = parse_fault_spec("delay:100ms-500ms").unwrap();
        assert!(matches!(r, FaultRule::Delay { min_ms: 100, max_ms: 500, url_pattern: None }));
    }

    #[test]
    fn parse_delay_with_url() {
        let r = parse_fault_spec("delay:200ms:/api/users").unwrap();
        match r {
            FaultRule::Delay { min_ms: 200, max_ms: 200, url_pattern: Some(p) } => {
                assert_eq!(p, "/api/users");
            }
            _ => panic!("unexpected rule"),
        }
    }

    #[test]
    fn parse_error_always() {
        let r = parse_fault_spec("error:503").unwrap();
        assert!(matches!(r, FaultRule::Error { status_code: 503, .. }));
    }

    #[test]
    fn parse_error_probability() {
        let r = parse_fault_spec("error:500:0.1").unwrap();
        match r {
            FaultRule::Error { status_code: 500, probability, url_pattern: None } => {
                assert!((probability - 0.1).abs() < 1e-9);
            }
            _ => panic!("unexpected rule"),
        }
    }

    #[test]
    fn parse_error_probability_and_url() {
        let r = parse_fault_spec("error:500:0.5:/api").unwrap();
        match r {
            FaultRule::Error { status_code: 500, probability, url_pattern: Some(p) } => {
                assert!((probability - 0.5).abs() < 1e-9);
                assert_eq!(p, "/api");
            }
            _ => panic!("unexpected rule"),
        }
    }

    #[test]
    fn parse_seconds_duration() {
        let r = parse_fault_spec("delay:2s").unwrap();
        assert!(matches!(r, FaultRule::Delay { min_ms: 2000, .. }));
    }

    #[test]
    fn url_pattern_matching() {
        let rule = FaultRule::Delay {
            min_ms: 100,
            max_ms: 100,
            url_pattern: Some("/api".to_string()),
        };
        assert!(rule.matches_url("http://example.com/api/users"));
        assert!(!rule.matches_url("http://example.com/health"));
    }

    #[test]
    fn no_url_pattern_matches_all() {
        let rule = FaultRule::Error {
            status_code: 503,
            probability: 1.0,
            url_pattern: None,
        };
        assert!(rule.matches_url("http://example.com/anything"));
    }
}
