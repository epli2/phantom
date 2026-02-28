use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use clap::{Parser, ValueEnum};
use phantom_capture::ProxyCaptureBackend;
use phantom_core::capture::CaptureBackend;
use phantom_core::storage::TraceStore;
use phantom_core::trace::HttpTrace;
use phantom_storage::FjallTraceStore;
use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, ValueEnum)]
enum Backend {
    /// MITM HTTP/HTTPS proxy (cross-platform, requires `http_proxy` env var).
    Proxy,
    /// LD_PRELOAD agent injected into a target process (Linux only).
    #[cfg(target_os = "linux")]
    Ldpreload,
}

#[derive(Debug, Clone, Default, ValueEnum)]
enum OutputMode {
    /// Interactive TUI (default).
    #[default]
    Tui,
    /// Stream captured traces as JSON Lines to stdout (one object per line).
    /// Useful for scripting and AI-driven workflows.
    Jsonl,
}

#[derive(Parser)]
#[command(
    name = "phantom",
    about = "Zero-instrumentation API observability",
    version
)]
struct Cli {
    /// Capture backend to use.
    #[arg(short, long, value_enum, default_value = "proxy")]
    backend: Backend,

    /// Output mode: interactive TUI or JSON Lines stream.
    #[arg(short, long, value_enum, default_value = "tui")]
    output: OutputMode,

    /// Port for the proxy backend.
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Directory for trace storage.
    #[arg(short, long)]
    data_dir: Option<PathBuf>,

    /// Path to libphantom_agent.so (required for --backend ldpreload).
    ///
    /// Example: ./target/debug/libphantom_agent.so
    #[arg(long, value_name = "PATH")]
    agent_lib: Option<PathBuf>,

    /// Command to run with LD_PRELOAD injected (required for --backend ldpreload).
    ///
    /// Everything after `--` is treated as the command.
    /// Example: `phantom --backend ldpreload --agent-lib ./libphantom_agent.so -- curl http://example.com`
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

    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    let store = Arc::new(FjallTraceStore::open(&data_dir)?);

    match cli.backend {
        Backend::Proxy => run_proxy(cli, store).await,
        #[cfg(target_os = "linux")]
        Backend::Ldpreload => run_ldpreload(cli, store).await,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Proxy backend
// ─────────────────────────────────────────────────────────────────────────────

async fn run_proxy(cli: Cli, store: Arc<FjallTraceStore>) -> anyhow::Result<()> {
    let mut backend = ProxyCaptureBackend::new(cli.port);
    let backend_name = backend.name().to_string();
    let trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;

    match cli.output {
        OutputMode::Tui => {
            eprintln!("phantom: proxy listening on 127.0.0.1:{}", cli.port);
            eprintln!("  Set your HTTP proxy to http://127.0.0.1:{}", cli.port);
            eprintln!(
                "  Example: curl -x http://127.0.0.1:{} http://httpbin.org/get",
                cli.port
            );
            eprintln!(
                "phantom: traces stored in {}",
                store_path_display(&cli.data_dir)
            );
            phantom_tui::run_tui(store, trace_rx, &backend_name).await?;
        }
        OutputMode::Jsonl => {
            eprintln!(
                "phantom: proxy listening on 127.0.0.1:{} [jsonl mode]",
                cli.port
            );
            run_jsonl_output(store, trace_rx, None).await?;
        }
    }

    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// LD_PRELOAD backend (Linux only)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn run_ldpreload(cli: Cli, store: Arc<FjallTraceStore>) -> anyhow::Result<()> {
    use phantom_capture::LdPreloadCaptureBackend;

    let agent_lib = cli.agent_lib.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "--agent-lib <PATH> is required for --backend ldpreload\n\
            Example: --agent-lib ./target/debug/libphantom_agent.so"
        )
    })?;

    if cli.command.is_empty() {
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
    eprintln!("  command   : {}", cli.command.join(" "));
    eprintln!(
        "phantom: traces stored in {}",
        store_path_display(&cli.data_dir)
    );

    // Spawn the target process with LD_PRELOAD and PHANTOM_SOCKET set.
    let child = std::process::Command::new(&cli.command[0])
        .args(&cli.command[1..])
        .env("LD_PRELOAD", &agent_lib)
        .env("PHANTOM_SOCKET", &socket_path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {e}", cli.command[0]))?;

    eprintln!("phantom: spawned PID {}", child.id());

    match cli.output {
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
