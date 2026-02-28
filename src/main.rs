use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use phantom_capture::ProxyCaptureBackend;
use phantom_core::capture::CaptureBackend;
use phantom_storage::FjallTraceStore;

#[derive(Parser)]
#[command(name = "phantom", about = "API observability tool", version)]
struct Cli {
    /// Port for the proxy capture backend
    #[arg(short, long, default_value = "8080")]
    port: u16,

    /// Directory for trace storage
    #[arg(short, long)]
    data_dir: Option<PathBuf>,
}

fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("phantom")
        .join("data")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    // Initialize storage
    let store = Arc::new(FjallTraceStore::open(&data_dir)?);

    // Initialize capture backend
    let mut backend = ProxyCaptureBackend::new(cli.port);
    let backend_name = backend.name().to_string();
    let trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;

    eprintln!("phantom: proxy listening on 127.0.0.1:{}", cli.port);
    eprintln!("phantom: traces stored in {}", data_dir.display());
    eprintln!("phantom: starting TUI...");

    // Run TUI (blocks until quit)
    phantom_tui::run_tui(store, trace_rx, &backend_name).await?;

    // Cleanup
    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(())
}
