use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use phantom_capture::ProxyCaptureBackend;
use phantom_core::capture::CaptureBackend;
use phantom_storage::FjallTraceStore;

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

    // Run TUI while child is alive (or until user quits).
    phantom_tui::run_tui(store, trace_rx, &backend_name).await?;

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
