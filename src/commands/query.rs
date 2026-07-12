use std::time::SystemTime;

use phantom_core::query::TraceQuery;
use phantom_core::storage::TraceStore;
use phantom_core::trace::{HttpTrace, SpanId, TraceId};
use phantom_core::view::{RenderOptions, TraceView};

use crate::cli::{FilterArgs, GetArgs, ListArgs, QueryFormat, SearchArgs};

/// Parse a `--since`/`--until` value: RFC3339 timestamp, or a relative
/// duration meaning "that long ago" (e.g. "30s", "10m", "2h").
fn parse_time(s: &str) -> anyhow::Result<SystemTime> {
    if let Ok(duration) = humantime::parse_duration(s) {
        return SystemTime::now()
            .checked_sub(duration)
            .ok_or_else(|| anyhow::anyhow!("relative time {s:?} is before the epoch"));
    }
    humantime::parse_rfc3339(s).map_err(|e| {
        anyhow::anyhow!(
            "invalid time {s:?}: {e} (expected RFC3339 like \"2026-07-12T10:00:00Z\" \
             or a relative duration like \"10m\")"
        )
    })
}

fn build_query(url: Option<String>, filter: &FilterArgs) -> anyhow::Result<TraceQuery> {
    let trace_id = filter
        .trace_id
        .as_deref()
        .map(|s| {
            TraceId::from_hex(s)
                .ok_or_else(|| anyhow::anyhow!("invalid trace ID {s:?}: expected 32 hex chars"))
        })
        .transpose()?;

    Ok(TraceQuery {
        methods: filter.methods.clone(),
        status: filter.status,
        url_contains: url,
        since: filter.since.as_deref().map(parse_time).transpose()?,
        until: filter.until.as_deref().map(parse_time).transpose()?,
        trace_id,
        limit: filter.limit,
        offset: filter.offset,
    })
}

fn render_options(max_body: usize, headers_only: bool, redact: &[String]) -> RenderOptions {
    RenderOptions {
        max_body: (max_body > 0).then_some(max_body),
        headers_only,
        redact_headers: redact.iter().map(|h| h.to_lowercase()).collect(),
    }
}

fn print_table(views: &[TraceView]) {
    println!(
        "{:<24}  {:<7}  {:>6}  {:>8}  {:<17}  URL",
        "TIME", "METHOD", "STATUS", "DURATION", "SPAN_ID"
    );
    for v in views {
        let time = humantime::format_rfc3339_millis(
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(v.timestamp_ms),
        );
        println!(
            "{:<24}  {:<7}  {:>6}  {:>6}ms  {:<17}  {}",
            time, v.method, v.status_code, v.duration_ms, v.span_id, v.url
        );
    }
}

fn output_traces(
    traces: &[HttpTrace],
    format: QueryFormat,
    opts: &RenderOptions,
) -> anyhow::Result<()> {
    let views: Vec<TraceView> = traces.iter().map(|t| TraceView::render(t, opts)).collect();
    match format {
        QueryFormat::Jsonl => {
            for view in &views {
                println!("{}", serde_json::to_string(view)?);
            }
        }
        QueryFormat::Json => println!("{}", serde_json::to_string_pretty(&views)?),
        QueryFormat::Table => print_table(&views),
    }
    Ok(())
}

pub fn list(store: &dyn TraceStore, args: ListArgs) -> anyhow::Result<()> {
    let query = build_query(args.url, &args.filter)?;
    let traces = store.query(&query)?;
    let opts = render_options(
        args.filter.max_body,
        args.filter.headers_only,
        &args.filter.redact_headers,
    );
    output_traces(&traces, args.filter.format, &opts)
}

pub fn search(store: &dyn TraceStore, args: SearchArgs) -> anyhow::Result<()> {
    let query = build_query(Some(args.pattern), &args.filter)?;
    let traces = store.query(&query)?;
    let opts = render_options(
        args.filter.max_body,
        args.filter.headers_only,
        &args.filter.redact_headers,
    );
    output_traces(&traces, args.filter.format, &opts)
}

/// Returns `false` (exit code 1) when the span ID is unknown.
pub fn get(store: &dyn TraceStore, args: GetArgs) -> anyhow::Result<bool> {
    let span_id = SpanId::from_hex(&args.span_id).ok_or_else(|| {
        anyhow::anyhow!("invalid span ID {:?}: expected 16 hex chars", args.span_id)
    })?;

    let Some(trace) = store.get_by_span_id(&span_id)? else {
        eprintln!("phantom: no trace found for span ID {}", args.span_id);
        return Ok(false);
    };

    let opts = render_options(args.max_body, args.headers_only, &[]);
    let view = TraceView::render(&trace, &opts);
    match args.format {
        QueryFormat::Jsonl => println!("{}", serde_json::to_string(&view)?),
        _ => println!("{}", serde_json::to_string_pretty(&view)?),
    }
    Ok(true)
}

pub fn stats(store: &dyn TraceStore, data_dir: &std::path::Path) -> anyhow::Result<()> {
    println!(
        "{}",
        serde_json::json!({
            "total_traces": store.count()?,
            "data_dir": data_dir.display().to_string(),
        })
    );
    Ok(())
}

/// Returns `false` (exit code 1) when `--yes` was not passed.
pub fn clear(store: &dyn TraceStore, yes: bool, quiet: bool) -> anyhow::Result<bool> {
    if !yes {
        eprintln!("phantom: refusing to delete all traces without --yes");
        return Ok(false);
    }
    store.clear()?;
    if !quiet {
        eprintln!("phantom: all traces cleared");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn test_parse_time_rfc3339() {
        let t = parse_time("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(t, UNIX_EPOCH + Duration::from_secs(1_767_225_600));
    }

    #[test]
    fn test_parse_time_relative_means_ago() {
        let before = SystemTime::now();
        let t = parse_time("10m").unwrap();
        let ago = before.duration_since(t).unwrap();
        // Within a small tolerance of exactly 10 minutes before "now".
        assert!(
            ago >= Duration::from_secs(599) && ago <= Duration::from_secs(601),
            "expected ~600s ago, got {ago:?}"
        );
    }

    #[test]
    fn test_parse_time_relative_units() {
        for (input, secs) in [("30s", 30), ("2h", 7200), ("1d", 86400)] {
            let t = parse_time(input).unwrap();
            let ago = SystemTime::now().duration_since(t).unwrap().as_secs();
            assert!(
                ago.abs_diff(secs) <= 1,
                "{input}: expected ~{secs}s ago, got {ago}s"
            );
        }
    }

    #[test]
    fn test_parse_time_rejects_garbage() {
        for input in ["", "yesterday", "2026-13-01T00:00:00Z", "10 parsecs"] {
            assert!(parse_time(input).is_err(), "{input:?} should not parse");
        }
    }

    #[test]
    fn test_build_query_rejects_bad_trace_id() {
        let filter = FilterArgs {
            methods: Vec::new(),
            status: None,
            since: None,
            until: None,
            trace_id: Some("not-hex".to_string()),
            limit: 50,
            offset: 0,
            format: QueryFormat::Jsonl,
            max_body: 0,
            headers_only: false,
            redact_headers: Vec::new(),
        };
        let err = build_query(None, &filter).unwrap_err();
        assert!(err.to_string().contains("invalid trace ID"));
    }

    #[test]
    fn test_render_options_zero_max_body_means_unlimited() {
        assert_eq!(render_options(0, false, &[]).max_body, None);
        assert_eq!(render_options(16, false, &[]).max_body, Some(16));
    }

    #[test]
    fn test_render_options_lowercases_redact_headers() {
        let opts = render_options(0, false, &["X-Api-Key".to_string()]);
        assert_eq!(opts.redact_headers, ["x-api-key"]);
    }
}
