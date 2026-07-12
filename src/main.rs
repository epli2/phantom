mod cli;
mod commands;
mod mcp;
mod runner;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use phantom_storage::FjallTraceStore;

use cli::{Backend, Cli, Commands, GlobalOpts, default_data_dir};

/// Maps a child process's exit status onto our own exit code:
/// the child's code clamped to u8, or 128+signal on Unix signal death.
fn exit_code_from_status(status: std::process::ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(code.clamp(0, 255) as u8);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitCode::from(128u8.saturating_add(signal.clamp(0, 127) as u8));
        }
    }
    ExitCode::FAILURE
}

/// Opens the trace store for a query command, adding a hint about fjall's
/// single-process lock when another phantom instance holds it.
fn open_store_for_query(data_dir: &std::path::Path) -> anyhow::Result<FjallTraceStore> {
    FjallTraceStore::open(data_dir).map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\
             hint: another phantom process (run/mcp) may hold the store lock on\n\
             {}. Stop it first, or query through the running MCP server.",
            data_dir.display()
        )
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();

    // All diagnostics go to stderr so stdout stays pure JSONL / JSON.
    let default_directive = if cli.quiet {
        "phantom=error"
    } else {
        "phantom=info"
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(default_directive.parse()?),
        )
        .init();

    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;
    let globals = GlobalOpts {
        quiet: cli.quiet,
        data_dir: data_dir.clone(),
    };

    match cli.command {
        Commands::Run(args) => {
            let store = Arc::new(FjallTraceStore::open(&data_dir)?);
            let child_status = match args.backend {
                Backend::Proxy => commands::run::run_proxy(&globals, args, store).await?,
                #[cfg(target_os = "linux")]
                Backend::Ldpreload => commands::run::run_ldpreload(&globals, args, store).await?,
            };
            Ok(child_status
                .map(exit_code_from_status)
                .unwrap_or(ExitCode::SUCCESS))
        }
        Commands::List(args) => {
            let store = open_store_for_query(&data_dir)?;
            commands::query::list(&store, args)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Search(args) => {
            let store = open_store_for_query(&data_dir)?;
            commands::query::search(&store, args)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Get(args) => {
            let store = open_store_for_query(&data_dir)?;
            let found = commands::query::get(&store, args)?;
            Ok(if found {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Commands::Stats => {
            let store = open_store_for_query(&data_dir)?;
            commands::query::stats(&store, &data_dir)?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Clear(args) => {
            let store = open_store_for_query(&data_dir)?;
            let cleared = commands::query::clear(&store, args.yes, globals.quiet)?;
            Ok(if cleared {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Commands::Mcp => {
            let store = Arc::new(open_store_for_query(&data_dir)?);
            mcp::run_mcp(store, data_dir).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}
