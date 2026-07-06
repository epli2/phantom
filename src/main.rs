use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use clap::{Parser, Subcommand, ValueEnum};
use phantom_capture::{CaPaths, FaultConfig, ProxyCaptureBackend};
use phantom_core::capture::CaptureBackend;
use phantom_core::storage::TraceStore;
use phantom_core::trace::HttpTrace;
use phantom_storage::FjallTraceStore;
use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// Embedded proxy preload script (Node.js transparent injection)
// ─────────────────────────────────────────────────────────────────────────────

/// The proxy-preload.js content, embedded at compile time.
/// Written to a temp file when tracing Node.js processes via `phantom -- node …`.
const NODE_PROXY_PRELOAD: &str = include_str!("../tests/apps/node-app/proxy-preload.js");

/// The Java Agent JAR, embedded at compile time.
/// Written to a temp file when tracing Java processes via `phantom -- java …`.
const JAVA_AGENT_JAR: &[u8] = include_bytes!("../crates/phantom-java-agent/phantom-java-agent.jar");

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, ValueEnum)]
enum Backend {
    /// MITM proxy — captures HTTP + HTTPS, cross-platform. Node.js HTTPS injected automatically.
    Proxy,
    /// LD_PRELOAD agent — plain HTTP only, Linux only. No proxy config needed.
    #[cfg(target_os = "linux")]
    Ldpreload,
}

#[derive(Debug, Clone, Default, ValueEnum)]
enum OutputMode {
    /// Interactive terminal UI with trace list and detail view.
    #[default]
    Tui,
    /// Stream traces as JSON Lines to stdout; auto-exits when child process finishes.
    Jsonl,
}

#[derive(Parser)]
#[command(
    name = "phantom",
    about = "Zero-instrumentation HTTP/HTTPS API observability tool",
    long_about = "phantom — Zero-instrumentation HTTP/HTTPS API observability\n\
\n\
Captures every HTTP and HTTPS request/response made by a target process\n\
and displays them in an interactive TUI or streams them as JSON Lines.\n\
The target application requires NO code changes.\n\
\n\
━━━ CAPTURE BACKENDS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
\n\
  proxy  (default, cross-platform)\n\
    Starts a MITM proxy on 127.0.0.1:<PORT>.  Intercepts HTTP and HTTPS.\n\
\n\
    • Node.js  (`phantom -- node app.js`)\n\
      proxy-preload.js is injected automatically via --require.  Both http://\n\
      and https:// are captured with zero application changes.\n\
\n\
    • Other commands  (`phantom -- curl http://api.example.com/v1`)\n\
      HTTP_PROXY / http_proxy is set automatically.  Plain HTTP is captured.\n\
      HTTPS requires the application to honour HTTP_PROXY CONNECT tunnelling.\n\
\n\
    • Manual  (start phantom alone, then configure your app)\n\
      Set HTTP_PROXY=http://127.0.0.1:8080 in the target process yourself.\n\
\n\
  ldpreload  (Linux only)\n\
    Injects libphantom_agent.so via LD_PRELOAD.  Hooks send/recv/close at\n\
    the libc level.  No proxy configuration required.  Plain HTTP only\n\
    (HTTPS traffic is already encrypted at the socket layer).\n\
\n\
━━━ OUTPUT MODES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
\n\
  tui   (default) — Interactive terminal UI with trace list + detail view.\n\
\n\
  jsonl — One JSON object per line on stdout.  phantom exits automatically\n\
          when the child process exits (ideal for scripting and AI agents).\n\
\n\
  JSONL record schema (all fields always present unless marked optional):\n\
    trace_id          string   W3C-compatible 128-bit trace ID (hex, 32 chars)\n\
    span_id           string   64-bit span ID (hex, 16 chars)\n\
    timestamp_ms      number   Unix epoch milliseconds — request start time\n\
    duration_ms       number   Round-trip latency in milliseconds\n\
    method            string   HTTP verb: \"GET\", \"POST\", \"PUT\", \"DELETE\", …\n\
    url               string   Full request URL (scheme + host + path + query)\n\
    status_code       number   HTTP response status code (200, 404, 500, …)\n\
    protocol_version  string   HTTP version string, e.g. \"HTTP/1.1\"\n\
    request_headers   object   Lower-cased header names → values\n\
    response_headers  object   Lower-cased header names → values\n\
    request_body      string?  UTF-8 decoded body; omitted when empty\n\
    response_body     string?  UTF-8 decoded body; omitted when empty\n\
    source_addr       string?  Client socket address, e.g. \"127.0.0.1:54321\"\n\
    dest_addr         string?  Server socket address, e.g. \"93.184.216.34:443\"",
    after_long_help = "EXAMPLES\n\
\n\
  ┌─ Proxy mode (default) ──────────────────────────────────────────────────┐\n\
\n\
  # Trace a Node.js app — HTTP + HTTPS captured, zero app changes:\n\
  phantom -- node app.js\n\
\n\
  # Stream traces as JSONL for scripting / AI analysis:\n\
  phantom --output jsonl -- node app.js\n\
\n\
  # Allow self-signed TLS certs on backend servers:\n\
  phantom --insecure --output jsonl -- node app.js\n\
\n\
  # Trace any command (plain HTTP only, HTTPS if app uses HTTP_PROXY CONNECT):\n\
  phantom -- curl http://api.example.com/v1/users\n\
\n\
  # Start proxy only, configure target app manually:\n\
  phantom\n\
  # then in another shell:\n\
  HTTP_PROXY=http://127.0.0.1:8080 node app.js\n\
\n\
  # Custom port, custom data directory:\n\
  phantom --port 9090 --data-dir ./traces -- node app.js\n\
\n\
  └─────────────────────────────────────────────────────────────────────────┘\n\
\n\
  ┌─ LD_PRELOAD mode (Linux only) ──────────────────────────────────────────┐\n\
\n\
  # Build the agent first:\n\
  cargo build -p phantom-agent\n\
\n\
  # Trace a process (plain HTTP only):\n\
  phantom --backend ldpreload \\\n\
          --agent-lib ./target/debug/libphantom_agent.so \\\n\
          -- curl http://api.example.com/v1/users\n\
\n\
  └─────────────────────────────────────────────────────────────────────────┘\n\
\n\
  ┌─ Consume JSONL from another process ────────────────────────────────────┐\n\
\n\
  phantom --output jsonl -- node app.js \\\n\
    | jq 'select(.status_code >= 400)'          # filter errors\n\
\n\
  phantom --output jsonl -- node app.js \\\n\
    | jq '{method,url,status_code,duration_ms}' # compact summary\n\
\n\
  └─────────────────────────────────────────────────────────────────────────┘",
    version,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Capture traffic from a target process (default when no subcommand is given).
    ///
    /// `phantom run -- node app.js` and the legacy `phantom -- node app.js`
    /// are equivalent; see `phantom --help` for the full capture guide.
    Run(RunArgs),
    /// Manage the CA certificate used for HTTPS interception.
    ///
    /// The CA is generated once and persisted under <DATA_DIR>/ca. Clients
    /// spawned via `phantom -- CMD` trust it automatically through environment
    /// variables (CURL_CA_BUNDLE, NODE_EXTRA_CA_CERTS, …); use this command to
    /// trust it from apps that phantom does not spawn (e.g. browsers).
    Cert(CertArgs),
}

#[derive(clap::Args)]
struct CertArgs {
    /// Directory where phantom stores data (the CA lives in <DIR>/ca).
    /// Defaults to the platform data directory, e.g. ~/.local/share/phantom/data.
    #[arg(short, long)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    action: CertAction,
}

#[derive(Subcommand)]
enum CertAction {
    /// Print the absolute path of the CA certificate (PEM).
    Path,
    /// Print the CA certificate PEM to stdout.
    Print,
    /// Copy the CA certificate to a file and show OS trust-store instructions.
    Export {
        /// Output file path.
        #[arg(long, default_value = "phantom-ca.cert.pem")]
        out: PathBuf,
    },
}

#[derive(clap::Args)]
struct RunArgs {
    /// Capture backend: 'proxy' (MITM, cross-platform) or 'ldpreload' (Linux, plain HTTP only).
    #[arg(short, long, value_enum, default_value = "proxy")]
    backend: Backend,

    /// Output mode: 'tui' opens the interactive UI; 'jsonl' streams one trace
    /// per line to stdout and exits when the child process finishes.
    #[arg(short, long, value_enum, default_value = "tui")]
    output: OutputMode,

    /// TCP port the proxy listens on.
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Disable TLS certificate verification for connections to backend servers.
    /// Use when tracing apps that talk to servers with self-signed certificates.
    #[arg(long, default_value = "false")]
    insecure: bool,

    /// Directory where captured traces are persisted (Fjall key-value store).
    /// Defaults to the platform data directory, e.g. ~/.local/share/phantom/data.
    #[arg(short, long)]
    data_dir: Option<PathBuf>,

    /// Path to libphantom_agent.so  [required for --backend ldpreload]
    ///
    /// Build with: cargo build -p phantom-agent
    /// Then pass:  --agent-lib ./target/debug/libphantom_agent.so
    #[arg(long, value_name = "PATH")]
    agent_lib: Option<PathBuf>,

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
    fault: Vec<String>,

    /// Command to spawn and trace (everything after `--`).
    ///
    /// proxy mode:     HTTP_PROXY is set automatically; Node.js additionally
    ///                 gets proxy-preload.js injected via --require (captures HTTPS too).
    /// ldpreload mode: LD_PRELOAD + PHANTOM_SOCKET are set automatically.
    #[arg(last = true, value_name = "CMD")]
    command: Vec<String>,
}

fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("phantom")
        .join("data")
}

// ─────────────────────────────────────────────────────────────────────────────
// JSONL output
// ─────────────────────────────────────────────────────────────────────────────

/// Human-friendly, fully serializable representation of an `HttpTrace`.
/// Emitted as one JSON object per line to stdout in `--output jsonl` mode.
#[derive(Serialize)]
struct JsonlTrace {
    /// Unix timestamp of the request in milliseconds.
    timestamp_ms: u64,
    /// Round-trip duration in milliseconds.
    duration_ms: u64,
    /// HTTP method ("GET", "POST", …).
    method: String,
    /// Full request URL.
    url: String,
    /// HTTP response status code.
    status_code: u16,
    /// Request headers (lower-cased keys).
    request_headers: std::collections::HashMap<String, String>,
    /// Response headers (lower-cased keys).
    response_headers: std::collections::HashMap<String, String>,
    /// Request body decoded as UTF-8 (replacement chars for non-UTF-8 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    request_body: Option<String>,
    /// Response body decoded as UTF-8 (replacement chars for non-UTF-8 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body: Option<String>,
    /// Source socket address, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_addr: Option<String>,
    /// Destination socket address, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    dest_addr: Option<String>,
    /// HTTP protocol version string (e.g. "HTTP/1.1").
    protocol_version: String,
    /// 128-bit W3C trace ID (hex).
    trace_id: String,
    /// 64-bit span ID (hex).
    span_id: String,
}

fn body_to_str(body: &Option<Vec<u8>>) -> Option<String> {
    body.as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

fn trace_to_jsonl(t: &HttpTrace) -> JsonlTrace {
    let timestamp_ms = t
        .timestamp
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    JsonlTrace {
        timestamp_ms,
        duration_ms: t.duration.as_millis() as u64,
        method: t.method.to_string(),
        url: t.url.clone(),
        status_code: t.status_code,
        request_headers: t.request_headers.clone(),
        response_headers: t.response_headers.clone(),
        request_body: body_to_str(&t.request_body),
        response_body: body_to_str(&t.response_body),
        source_addr: t.source_addr.clone(),
        dest_addr: t.dest_addr.clone(),
        protocol_version: t.protocol_version.clone(),
        trace_id: t.trace_id.to_string(),
        span_id: t.span_id.to_string(),
    }
}

/// Runs the JSONL output loop: each captured trace is serialized and written to
/// stdout as a single JSON object followed by a newline.
///
/// Exits when:
/// - The trace channel is closed (sender dropped),
/// - Ctrl-C is received, or
/// - The optional `child` process exits (ldpreload mode).
async fn run_jsonl_output(
    store: Arc<FjallTraceStore>,
    mut trace_rx: tokio::sync::mpsc::Receiver<HttpTrace>,
    child: Option<std::process::Child>,
) -> anyhow::Result<()> {
    // Spawn a background thread to wait() on the child so we don't block the
    // async executor.  Signal completion via a oneshot channel.
    let mut child_done: Option<tokio::sync::oneshot::Receiver<()>> = if let Some(mut c) = child {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = c.wait();
            let _ = tx.send(());
        });
        Some(rx)
    } else {
        None
    };

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            maybe_trace = trace_rx.recv() => {
                match maybe_trace {
                    Some(t) => {
                        store.insert(&t).ok();
                        println!("{}", serde_json::to_string(&trace_to_jsonl(&t))?);
                    }
                    None => break,
                }
            }
            _ = &mut ctrl_c => break,
            // When the child exits, wait briefly for the backend to flush any
            // in-flight datagrams, then drain whatever arrived.
            _ = async {
                if let Some(rx) = child_done.as_mut() {
                    let _ = rx.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                while let Ok(t) = trace_rx.try_recv() {
                    store.insert(&t).ok();
                    println!("{}", serde_json::to_string(&trace_to_jsonl(&t))?);
                }
                break;
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("phantom=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    // Subcommand-less invocation (`phantom -- node app.js`) behaves exactly
    // like `phantom run -- node app.js` for backward compatibility.
    let args = match cli.command {
        Some(Command::Cert(cert)) => return run_cert(cert),
        Some(Command::Run(run)) => run,
        None => cli.run,
    };

    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    let store = Arc::new(FjallTraceStore::open(&data_dir)?);

    match args.backend {
        Backend::Proxy => run_proxy(args, store).await,
        #[cfg(target_os = "linux")]
        Backend::Ldpreload => run_ldpreload(args, store).await,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cert subcommand
// ─────────────────────────────────────────────────────────────────────────────

fn run_cert(args: CertArgs) -> anyhow::Result<()> {
    let data_dir = args.data_dir.unwrap_or_else(default_data_dir);
    let ca_dir = data_dir.join("ca");
    let ca = phantom_capture::ensure_ca(&ca_dir).map_err(|e| anyhow::anyhow!("{e}"))?;

    match args.action {
        CertAction::Path => println!("{}", ca.cert_path.display()),
        CertAction::Print => print!("{}", std::fs::read_to_string(&ca.cert_path)?),
        CertAction::Export { out } => {
            std::fs::copy(&ca.cert_path, &out)
                .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", out.display()))?;
            eprintln!("phantom: CA certificate exported to {}", out.display());
            eprintln!();
            eprintln!("Processes spawned via `phantom -- CMD` trust this CA automatically.");
            eprintln!("To trust it system-wide (for apps phantom does not spawn):");
            eprintln!();
            eprintln!("  macOS:");
            eprintln!("    sudo security add-trusted-cert -d -r trustRoot \\");
            eprintln!(
                "      -k /Library/Keychains/System.keychain {}",
                out.display()
            );
            eprintln!("  Ubuntu/Debian:");
            eprintln!(
                "    sudo cp {} /usr/local/share/ca-certificates/phantom-ca.crt",
                out.display()
            );
            eprintln!("    sudo update-ca-certificates");
            eprintln!("  Windows (PowerShell, admin):");
            eprintln!("    certutil -addstore -f ROOT {}", out.display());
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Fault injection
// ─────────────────────────────────────────────────────────────────────────────

fn build_fault_config(specs: &[String]) -> anyhow::Result<FaultConfig> {
    let mut rules = Vec::new();
    for spec in specs {
        let rule = phantom_capture::parse_fault_spec(spec)
            .map_err(|e| anyhow::anyhow!("--fault {spec:?}: {e}"))?;
        rules.push(rule);
    }
    Ok(FaultConfig { rules })
}

// ─────────────────────────────────────────────────────────────────────────────
// Proxy backend
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard that deletes a temporary script file on drop.
struct TempScript(PathBuf);

impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Returns `true` if `exe` (path or bare name) resolves to `node` or `nodejs`.
fn is_node_command(exe: &str) -> bool {
    let base = Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(exe);
    base == "node" || base == "nodejs"
}

/// Returns `true` if `exe` (path or bare name) resolves to `java` or `javaw`.
fn is_java_command(exe: &str) -> bool {
    let base = Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(exe);
    base == "java" || base == "javaw"
}

/// Locate the baseline CA roots to include in the combined phantom bundle:
/// a bundle the user already points at via environment, else the system one.
fn find_base_ca_bundle() -> Option<PathBuf> {
    for key in ["CURL_CA_BUNDLE", "SSL_CERT_FILE", "REQUESTS_CA_BUNDLE"] {
        if let Some(p) = std::env::var_os(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    [
        "/etc/ssl/certs/ca-certificates.crt", // Debian/Ubuntu
        "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora/RHEL
        "/etc/ssl/ca-bundle.pem",             // openSUSE
        "/etc/ssl/cert.pem",                  // macOS (LibreSSL), Alpine, BSD
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Write `<ca_dir>/phantom-ca-bundle.pem`: previously trusted roots (user or
/// system bundle, plus any NODE_EXTRA_CA_CERTS) with the phantom CA appended.
/// A strict superset of what the child already trusted, so pointing trust
/// variables at it never *removes* trust — it only adds the phantom CA.
/// Rebuilt on every start so trust-store updates are picked up.
fn build_ca_bundle(ca_dir: &Path, ca: &CaPaths) -> anyhow::Result<PathBuf> {
    let bundle_path = ca_dir.join("phantom-ca-bundle.pem");
    let mut bundle = String::new();

    let mut append_file = |path: &Path| match std::fs::read_to_string(path) {
        Ok(pem) => {
            bundle.push_str(&pem);
            if !bundle.ends_with('\n') {
                bundle.push('\n');
            }
        }
        Err(e) => eprintln!("phantom: could not read CA file {}: {e}", path.display()),
    };

    match find_base_ca_bundle() {
        Some(base) => append_file(&base),
        None => eprintln!(
            "phantom: no system CA bundle found; the traced process will only \
             trust the phantom CA"
        ),
    }

    // Extra CAs the user already gave to Node.js stay trusted.
    if let Some(extra) = std::env::var_os("NODE_EXTRA_CA_CERTS") {
        let extra = PathBuf::from(extra);
        if extra.is_file() {
            append_file(&extra);
        }
    }

    bundle.push_str(&std::fs::read_to_string(&ca.cert_path)?);
    std::fs::write(&bundle_path, bundle)?;

    Ok(bundle_path)
}

/// Spawns `command` as a child process routed through the phantom proxy.
///
/// * `HTTP_PROXY` / `HTTPS_PROXY` (and lowercase variants) are set so both
///   plain and TLS traffic is captured.
/// * CA trust variables (CURL_CA_BUNDLE, SSL_CERT_FILE, REQUESTS_CA_BUNDLE,
///   NODE_EXTRA_CA_CERTS, DENO_CERT) point at the phantom CA so clients can
///   verify MITM'd certificates without disabling TLS verification.
/// * For Node.js executables the embedded proxy-preload script is written to a
///   temp file and prepended as `--require <path>` so HTTPS is also captured
///   without touching the application source.
/// * For Java executables, the phantom-java-agent.jar is injected via -javaagent
///   to force proxy settings and bypass SSL verification globally.
///
/// Returns `(child, Option<TempScript>)`.  The `TempScript` must be kept alive
/// until after the child exits so the file is not deleted prematurely.
fn spawn_proxy_child(
    command: &[String],
    proxy_port: u16,
    ca_bundle: &Path,
) -> anyhow::Result<(std::process::Child, Option<TempScript>)> {
    let exe = &command[0];
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");

    let mut temp_script: Option<TempScript> = None;
    let mut actual_args = command[1..].to_vec();

    if is_node_command(exe) {
        // Write the embedded preload script to a temp file.
        let script_path =
            std::env::temp_dir().join(format!("phantom-preload-{}.js", std::process::id()));
        std::fs::write(&script_path, NODE_PROXY_PRELOAD)
            .map_err(|e| anyhow::anyhow!("failed to write proxy preload script: {e}"))?;
        temp_script = Some(TempScript(script_path.clone()));

        // Prepend --require <script> before the rest of the args.
        let mut args = vec![
            "--require".to_string(),
            script_path.to_string_lossy().into_owned(),
        ];
        args.extend_from_slice(&command[1..]);
        actual_args = args;
    }

    let mut cmd = std::process::Command::new(exe);
    cmd.args(&actual_args);

    // Route both plain and TLS traffic through the proxy. These are essential
    // for capture, so an existing value is overridden (with a notice).
    //
    // Node.js is the exception: proxy-preload.js routes HTTPS itself via a
    // CONNECT tunnel agent. Setting HTTPS_PROXY there would also trigger
    // axios's own env-based proxying of https:// URLs, which conflicts with
    // the preload's patched https.request.
    let proxy_keys: &[&str] = if is_node_command(exe) {
        &["HTTP_PROXY", "http_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"]
    };
    for key in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "https_proxy"] {
        if proxy_keys.contains(&key) {
            if let Some(old) = std::env::var_os(key)
                && old != *OsStr::new(&proxy_url)
            {
                eprintln!("phantom: overriding {key} for the traced process");
            }
            cmd.env(key, &proxy_url);
        } else {
            // Inherited values must not leak into the child either.
            cmd.env_remove(key);
        }
    }

    // For Java processes, additionally inject proxy settings and the Java
    // Agent via JAVA_TOOL_OPTIONS. The JVM does not read HTTP_PROXY /
    // HTTPS_PROXY itself, so this is required for java.net-based clients to
    // route through phantom at all.
    if is_java_command(exe) {
        // Write the embedded Java Agent to a temp file.
        let agent_path =
            std::env::temp_dir().join(format!("phantom-agent-{}.jar", std::process::id()));
        std::fs::write(&agent_path, JAVA_AGENT_JAR)
            .map_err(|e| anyhow::anyhow!("failed to write java agent jar: {e}"))?;
        temp_script = Some(TempScript(agent_path.clone()));

        let jvm_opts = format!(
            " -Dhttp.proxyHost=127.0.0.1 -Dhttp.proxyPort={proxy_port} \
              -Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort={proxy_port} \
              -Dhttp.nonProxyHosts= -Dhttps.nonProxyHosts= \
              -javaagent:{}",
            agent_path.display()
        );

        let existing = std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default();
        cmd.env("JAVA_TOOL_OPTIONS", format!("{existing}{jvm_opts}"));
    }

    // Other inherited proxy variables would make the child bypass phantom
    // (NO_PROXY) or send traffic to a different proxy (ALL_PROXY, npm_config_*),
    // silently creating capture blind spots — clear them.
    for key in [
        "NO_PROXY",
        "no_proxy",
        "ALL_PROXY",
        "all_proxy",
        "npm_config_proxy",
        "npm_config_http_proxy",
        "npm_config_https_proxy",
        "npm_config_no_proxy",
        "npm_config_noproxy",
    ] {
        if std::env::var_os(key).is_some() {
            eprintln!("phantom: clearing {key} for the traced process");
        }
        cmd.env_remove(key);
    }

    // Point common TLS stacks at the combined bundle (previously trusted
    // roots + phantom CA) so MITM'd certificates verify without disabling
    // TLS verification. The bundle is a superset of what was trusted before
    // (build_ca_bundle folds in any user-set bundle), so overriding is safe.
    // Java is unaffected by these (it uses the -javaagent trust-all bypass
    // above instead of these env-based TLS trust stores).
    for key in [
        "SSL_CERT_FILE",       // OpenSSL, Ruby, some Go builds
        "CURL_CA_BUNDLE",      // curl
        "REQUESTS_CA_BUNDLE",  // Python requests
        "NODE_EXTRA_CA_CERTS", // Node.js (additive)
        "DENO_CERT",           // Deno
    ] {
        if std::env::var_os(key).is_some() {
            eprintln!(
                "phantom: {key} was set; pointing it at the combined phantom CA \
                 bundle (previous roots included)"
            );
        }
        cmd.env(key, ca_bundle.as_os_str());
    }

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {e}", exe))?;

    Ok((child, temp_script))
}

async fn run_proxy(args: RunArgs, store: Arc<FjallTraceStore>) -> anyhow::Result<()> {
    let fault_config = build_fault_config(&args.fault)?;

    // Persist the MITM CA under <data-dir>/ca so it survives restarts and can
    // be trusted by clients (see `phantom cert --help`).
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    let ca_dir = data_dir.join("ca");
    let ca_paths = phantom_capture::ensure_ca(&ca_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let ca_bundle = build_ca_bundle(&ca_dir, &ca_paths)?;

    let mut backend = ProxyCaptureBackend::new(args.port, args.insecure)
        .with_faults(fault_config)
        .with_ca_dir(ca_dir);
    let backend_name = backend.name().to_string();
    let trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;

    // Optionally spawn a child command routed through the proxy.
    let child_and_script: Option<(std::process::Child, Option<TempScript>)> =
        if !args.command.is_empty() {
            // Wait for the proxy to be ready before spawning the child.
            wait_for_proxy(args.port).await?;
            let (child, ts) = spawn_proxy_child(&args.command, args.port, &ca_bundle)?;
            eprintln!(
                "phantom: spawned PID {} → {}",
                child.id(),
                args.command.join(" ")
            );
            Some((child, ts))
        } else {
            None
        };

    match args.output {
        OutputMode::Tui => {
            if args.command.is_empty() {
                eprintln!("phantom: proxy listening on 127.0.0.1:{}", args.port);
                eprintln!("  Set your HTTP proxy to http://127.0.0.1:{}", args.port);
                eprintln!(
                    "  Example: curl -x http://127.0.0.1:{} http://httpbin.org/get",
                    args.port
                );
                eprintln!("  HTTPS: trust the phantom CA — see `phantom cert export --help`");
            }
            eprintln!(
                "phantom: traces stored in {}",
                store_path_display(&args.data_dir)
            );
            phantom_tui::run_tui(store, trace_rx, &backend_name).await?;
        }
        OutputMode::Jsonl => {
            eprintln!(
                "phantom: proxy listening on 127.0.0.1:{} [jsonl mode]",
                args.port
            );
            // Split into child and script guard separately so the TempScript
            // is NOT dropped until after run_jsonl_output completes (the file
            // must exist while node is loading it via --require).
            let (child, _script_guard) = match child_and_script {
                Some((c, ts)) => (Some(c), ts),
                None => (None, None),
            };
            run_jsonl_output(store, trace_rx, child).await?;
            // _script_guard dropped here — temp file deleted after child exits.
        }
    }

    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Poll until the proxy port is accepting connections (or timeout after 5 s).
async fn wait_for_proxy(port: u16) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("proxy did not become ready on port {port} within 5 s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LD_PRELOAD backend (Linux only)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn run_ldpreload(args: RunArgs, store: Arc<FjallTraceStore>) -> anyhow::Result<()> {
    use phantom_capture::LdPreloadCaptureBackend;

    let agent_lib = args.agent_lib.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "--agent-lib <PATH> is required for --backend ldpreload\n\
            Example: --agent-lib ./target/debug/libphantom_agent.so"
        )
    })?;

    if args.command.is_empty() {
        anyhow::bail!(
            "A command to trace is required for --backend ldpreload.\n\
            Usage: phantom --backend ldpreload --agent-lib ./libphantom_agent.so -- curl http://example.com"
        );
    }

    // Generate a unique socket path for this run.
    let socket_path = std::env::temp_dir().join(format!(
        "phantom-{}.sock",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));

    let mut backend = LdPreloadCaptureBackend::new(socket_path.clone());
    let backend_name = backend.name().to_string();
    let trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!("phantom: ldpreload backend active");
    eprintln!("  agent lib : {}", agent_lib.display());
    eprintln!("  socket    : {}", socket_path.display());
    eprintln!("  command   : {}", args.command.join(" "));
    eprintln!(
        "phantom: traces stored in {}",
        store_path_display(&args.data_dir)
    );

    // Spawn the target process with LD_PRELOAD and PHANTOM_SOCKET set.
    let child = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .env("LD_PRELOAD", &agent_lib)
        .env("PHANTOM_SOCKET", &socket_path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {e}", args.command[0]))?;

    eprintln!("phantom: spawned PID {}", child.id());

    match args.output {
        OutputMode::Tui => {
            // In TUI mode the user quits manually; child runs in background.
            phantom_tui::run_tui(store, trace_rx, &backend_name).await?;
        }
        OutputMode::Jsonl => {
            // In JSONL mode we exit automatically when the child finishes.
            run_jsonl_output(store, trace_rx, Some(child)).await?;
        }
    }

    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn store_path_display(override_dir: &Option<PathBuf>) -> String {
    override_dir
        .clone()
        .unwrap_or_else(default_data_dir)
        .display()
        .to_string()
}
