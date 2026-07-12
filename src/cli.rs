use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use phantom_core::query::StatusRange;
use phantom_core::trace::HttpMethod;

#[derive(Debug, Clone, ValueEnum)]
pub enum Backend {
    /// MITM proxy — captures HTTP + HTTPS, cross-platform. Node.js HTTPS injected automatically.
    Proxy,
    /// LD_PRELOAD agent — captures HTTP + HTTPS, Linux only. No proxy config needed.
    #[cfg(target_os = "linux")]
    Ldpreload,
}

#[derive(Debug, Clone, Default, ValueEnum)]
pub enum OutputMode {
    /// Interactive terminal UI with trace list and detail view.
    #[default]
    Tui,
    /// Stream traces as JSON Lines to stdout; auto-exits when child process finishes.
    Jsonl,
}

/// Output format for query subcommands (`list`, `search`, `get`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum QueryFormat {
    /// One compact JSON object per line (pipe to jq).
    #[default]
    Jsonl,
    /// Pretty-printed JSON (array for list/search, object for get).
    Json,
    /// Human-readable fixed-width columns.
    Table,
}

#[derive(Parser)]
#[command(
    name = "phantom",
    about = "Zero-instrumentation HTTP/HTTPS API observability tool",
    long_about = "phantom — Zero-instrumentation HTTP/HTTPS API observability\n\
\n\
Captures every HTTP and HTTPS request/response made by a target process\n\
and displays them in an interactive TUI, streams them as JSON Lines, or\n\
serves them to AI coding agents over MCP.\n\
\n\
Typical workflows:\n\
\n\
  # Capture traffic from a command (TUI):\n\
  phantom run -- node app.js\n\
\n\
  # Capture and stream JSONL for scripting / AI analysis:\n\
  phantom run --output jsonl -- node app.js\n\
\n\
  # Query previously captured traces:\n\
  phantom list --status 5xx --since 10m\n\
  phantom get <SPAN_ID>\n\
\n\
  # Run as an MCP server (capture control + queries over stdio):\n\
  phantom mcp\n\
\n\
Note: the trace store is locked by a single phantom process at a time.\n\
Query subcommands work while no capture is running; while `phantom run`\n\
or `phantom mcp` is active, query through the MCP server instead.",
    version
)]
pub struct Cli {
    /// Directory where captured traces are persisted (Fjall key-value store).
    /// Defaults to the platform data directory, e.g. ~/.local/share/phantom/data.
    #[arg(short, long, global = true)]
    pub data_dir: Option<PathBuf>,

    /// Suppress status messages on stderr (machine-friendly output only).
    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Capture HTTP(S) traffic, optionally spawning a command to trace.
    Run(RunArgs),
    /// List captured traces (newest first) with filters.
    List(ListArgs),
    /// Show a single trace by span ID.
    Get(GetArgs),
    /// Shorthand for `list --url <PATTERN>`.
    Search(SearchArgs),
    /// Print trace store statistics as JSON.
    Stats,
    /// Delete all captured traces.
    Clear(ClearArgs),
    /// Run as an MCP (Model Context Protocol) server over stdio.
    ///
    /// Exposes capture control and trace queries as MCP tools for AI coding
    /// agents. Register with e.g.: claude mcp add phantom -- phantom mcp
    Mcp,
}

#[derive(Args)]
#[command(
    long_about = "Capture HTTP(S) traffic, optionally spawning a command to trace.\n\
\n\
━━━ CAPTURE BACKENDS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
\n\
  proxy  (default, cross-platform)\n\
    Starts a MITM proxy on 127.0.0.1:<PORT>.  Intercepts HTTP and HTTPS.\n\
\n\
    • Node.js  (`phantom run -- node app.js`)\n\
      proxy-preload.js is injected automatically via --require.  Both http://\n\
      and https:// are captured with zero application changes.\n\
\n\
    • PHP  (`phantom run -- php app.php`)\n\
      The MITM CA certificate is injected automatically via -d curl.cainfo=.\n\
      curl-based HTTP and HTTPS (incl. Guzzle's default handler) are captured\n\
      with zero application changes.  Requires PHP >= 5.3.7 for curl.cainfo;\n\
      only the curl extension is covered (not PHP streams).\n\
\n\
    • Other commands  (`phantom run -- curl http://api.example.com/v1`)\n\
      HTTP_PROXY / HTTPS_PROXY (and lowercase variants) are set automatically.\n\
      Plain HTTP is captured; HTTPS is captured if the application honours\n\
      these env vars for CONNECT tunnelling (as libcurl does by default).\n\
\n\
    • Manual  (start phantom alone, then configure your app)\n\
      Set HTTP_PROXY=http://127.0.0.1:8080 in the target process yourself.\n\
\n\
  ldpreload  (Linux only)\n\
    Injects libphantom_agent.so via LD_PRELOAD.  Hooks send/recv/close at\n\
    the libc level for plain HTTP, and OpenSSL SSL_write/SSL_read for HTTPS\n\
    (captured above the TLS layer, before encryption). No proxy config\n\
    required and no MITM certificate involved — works for any dynamically\n\
    linked process, language-agnostic (e.g. PHP's curl extension).\n\
\n\
━━━ OUTPUT MODES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
\n\
  tui   (default) — Interactive terminal UI with trace list + detail view.\n\
\n\
  jsonl — One JSON object per line on stdout.  phantom exits automatically\n\
          when the child process exits, propagating its exit code (ideal\n\
          for scripting and AI agents).\n\
\n\
  JSONL record schema (all fields always present unless marked optional):\n\
    trace_id                 string   W3C-compatible 128-bit trace ID (hex, 32 chars)\n\
    span_id                  string   64-bit span ID (hex, 16 chars)\n\
    timestamp_ms             number   Unix epoch milliseconds — request start time\n\
    duration_ms              number   Round-trip latency in milliseconds\n\
    method                   string   HTTP verb: \"GET\", \"POST\", \"PUT\", \"DELETE\", …\n\
    url                      string   Full request URL (scheme + host + path + query)\n\
    status_code              number   HTTP response status code (200, 404, 500, …)\n\
    protocol_version         string   HTTP version string, e.g. \"HTTP/1.1\"\n\
    request_headers          object   Lower-cased header names → values\n\
    response_headers         object   Lower-cased header names → values\n\
    request_body             string?  UTF-8 decoded body; omitted when empty\n\
    response_body            string?  UTF-8 decoded body; omitted when empty\n\
    request_body_bytes       number?  Original body size; present when a body existed\n\
    response_body_bytes      number?  Original body size; present when a body existed\n\
    request_body_truncated   bool?    Present (true) when --max-body truncated the body\n\
    response_body_truncated  bool?    Present (true) when --max-body truncated the body\n\
    source_addr              string?  Client socket address, e.g. \"127.0.0.1:54321\"\n\
    dest_addr                string?  Server socket address, e.g. \"93.184.216.34:443\"",
    after_long_help = "EXAMPLES\n\
\n\
  # Trace a Node.js app — HTTP + HTTPS captured, zero app changes:\n\
  phantom run -- node app.js\n\
\n\
  # Stream traces as JSONL for scripting / AI analysis:\n\
  phantom run --output jsonl -- node app.js\n\
\n\
  # Truncate bodies to keep output small:\n\
  phantom run --output jsonl --max-body 256 -- node app.js\n\
\n\
  # Filter errors with jq:\n\
  phantom run --output jsonl -- node app.js | jq 'select(.status_code >= 400)'\n\
\n\
  # LD_PRELOAD mode (Linux only):\n\
  cargo build -p phantom-agent\n\
  phantom run --backend ldpreload \\\n\
          --agent-lib ./target/debug/libphantom_agent.so \\\n\
          -- curl http://api.example.com/v1/users"
)]
pub struct RunArgs {
    /// Capture backend: 'proxy' (MITM, cross-platform) or 'ldpreload' (Linux, HTTP + HTTPS).
    #[arg(short, long, value_enum, default_value = "proxy")]
    pub backend: Backend,

    /// Output mode: 'tui' opens the interactive UI; 'jsonl' streams one trace
    /// per line to stdout and exits with the child's exit code when it finishes.
    #[arg(short, long, value_enum, default_value = "tui")]
    pub output: OutputMode,

    /// TCP port the proxy listens on.
    #[arg(short, long, default_value = "8080")]
    pub port: u16,

    /// Disable TLS certificate verification for connections to backend servers.
    /// Use when tracing apps that talk to servers with self-signed certificates.
    #[arg(long, default_value = "false")]
    pub insecure: bool,

    /// Path to libphantom_agent.so  [required for --backend ldpreload]
    ///
    /// Build with: cargo build -p phantom-agent
    /// Then pass:  --agent-lib ./target/debug/libphantom_agent.so
    #[arg(long, value_name = "PATH")]
    pub agent_lib: Option<PathBuf>,

    /// Inject faults into proxied requests (proxy backend only).
    ///
    /// SPEC formats:
    ///   delay:100ms              fixed 100 ms delay on all requests
    ///   delay:100ms-500ms        random delay in the given range
    ///   delay:200ms:/api         delay only URLs containing "/api"
    ///   error:503                return HTTP 503 for all requests
    ///   error:503:0.5            return HTTP 503 with 50% probability
    ///   error:500:0.1:/api       10% chance of HTTP 500 on URLs containing "/api"
    ///
    /// Rules are applied in order; delays and errors can be combined.
    /// Repeat the flag to add multiple rules:
    ///   --fault delay:50ms --fault error:500:0.1
    #[arg(long, value_name = "SPEC")]
    pub fault: Vec<String>,

    /// Truncate request/response bodies to N bytes in JSONL output
    /// (0 = unlimited). Truncated records carry `*_body_truncated: true`
    /// and the original size in `*_body_bytes`.
    #[arg(long, value_name = "N", default_value = "0")]
    pub max_body: usize,

    /// Omit request/response bodies from JSONL output entirely
    /// (original sizes still reported in `*_body_bytes`).
    #[arg(long)]
    pub headers_only: bool,

    /// Command to spawn and trace (everything after `--`).
    ///
    /// proxy mode:     HTTP_PROXY is set automatically; Node.js additionally
    ///                 gets proxy-preload.js injected via --require (captures HTTPS too).
    /// ldpreload mode: LD_PRELOAD + PHANTOM_SOCKET are set automatically.
    #[arg(last = true, value_name = "CMD")]
    pub command: Vec<String>,
}

/// Filter and rendering flags shared by `list` and `search`.
#[derive(Args)]
pub struct FilterArgs {
    /// Only these HTTP methods (repeatable): --method GET --method POST
    #[arg(long = "method", value_name = "METHOD")]
    pub methods: Vec<HttpMethod>,

    /// Status code filter: exact ("404"), class ("4xx"), or range ("400-499").
    #[arg(long, value_name = "RANGE")]
    pub status: Option<StatusRange>,

    /// Only traces newer than this: RFC3339 ("2026-07-12T10:00:00Z")
    /// or a relative duration ago ("30s", "10m", "2h").
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,

    /// Only traces older than this: RFC3339 or a relative duration ago.
    #[arg(long, value_name = "TIME")]
    pub until: Option<String>,

    /// Only spans belonging to this 32-char hex trace ID.
    #[arg(long, value_name = "HEX32")]
    pub trace_id: Option<String>,

    /// Maximum number of traces to return.
    #[arg(long, default_value = "50")]
    pub limit: usize,

    /// Number of matching traces to skip (for pagination).
    #[arg(long, default_value = "0")]
    pub offset: usize,

    /// Output format.
    #[arg(long, value_enum, default_value = "jsonl")]
    pub format: QueryFormat,

    /// Truncate bodies to N bytes (0 = unlimited).
    #[arg(long, value_name = "N", default_value = "1024")]
    pub max_body: usize,

    /// Omit bodies entirely (original sizes still reported).
    #[arg(long)]
    pub headers_only: bool,

    /// Replace this header's value with "[redacted]" (repeatable).
    #[arg(long = "redact-header", value_name = "NAME")]
    pub redact_headers: Vec<String>,
}

#[derive(Args)]
#[command(after_long_help = "EXAMPLES\n\
\n\
  # Recent failures, compact:\n\
  phantom list --status 5xx --limit 10 --format table\n\
\n\
  # POSTs to the users API in the last 10 minutes, as JSONL for jq:\n\
  phantom list --method POST --url /api/users --since 10m | jq .url\n\
\n\
  # Everything from one distributed trace:\n\
  phantom list --trace-id 0123456789abcdef0123456789abcdef")]
pub struct ListArgs {
    /// Only URLs containing this substring (case-insensitive).
    #[arg(long, value_name = "SUBSTR")]
    pub url: Option<String>,

    #[command(flatten)]
    pub filter: FilterArgs,
}

#[derive(Args)]
pub struct SearchArgs {
    /// URL substring to search for (case-insensitive).
    pub pattern: String,

    #[command(flatten)]
    pub filter: FilterArgs,
}

#[derive(Args)]
pub struct GetArgs {
    /// 16-character hex span ID (as shown by `phantom list`).
    pub span_id: String,

    /// Output format ('json' is pretty-printed; 'jsonl' is one compact line).
    #[arg(long, value_enum, default_value = "json")]
    pub format: QueryFormat,

    /// Truncate bodies to N bytes (0 = unlimited).
    #[arg(long, value_name = "N", default_value = "0")]
    pub max_body: usize,

    /// Omit bodies entirely (original sizes still reported).
    #[arg(long)]
    pub headers_only: bool,
}

#[derive(Args)]
pub struct ClearArgs {
    /// Confirm deletion (required; refuses to run without it).
    #[arg(long)]
    pub yes: bool,
}

/// Resolved global flags passed to command handlers.
pub struct GlobalOpts {
    pub quiet: bool,
    pub data_dir: PathBuf,
}

pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("phantom")
        .join("data")
}
