use std::process::ExitStatus;
use std::sync::Arc;

use phantom_capture::ProxyCaptureBackend;
use phantom_core::capture::CaptureBackend;
use phantom_core::storage::TraceStore;
use phantom_core::trace::HttpTrace;
use phantom_core::view::{RenderOptions, TraceView};
use phantom_storage::FjallTraceStore;
use tracing::warn;

use crate::cli::{GlobalOpts, OutputMode, RunArgs};
use crate::runner::{
    TempScript, build_fault_config, loopback_safe, spawn_proxy_child, wait_for_proxy,
};

/// Render options for the JSONL stream, from `run` flags.
fn jsonl_render_options(args: &RunArgs) -> RenderOptions {
    RenderOptions {
        max_body: (args.max_body > 0).then_some(args.max_body),
        headers_only: args.headers_only,
        redact_headers: Vec::new(),
    }
}

/// Runs the JSONL output loop: each captured trace is serialized and written to
/// stdout as a single JSON object followed by a newline.
///
/// Exits when:
/// - The trace channel is closed (sender dropped),
/// - Ctrl-C is received, or
/// - The optional `child` process exits.
///
/// Returns the child's exit status (when a child was spawned and exited) so
/// the caller can propagate its exit code.
async fn run_jsonl_output(
    store: Arc<FjallTraceStore>,
    mut trace_rx: tokio::sync::mpsc::Receiver<HttpTrace>,
    child: Option<std::process::Child>,
    opts: &RenderOptions,
    quiet: bool,
) -> anyhow::Result<Option<ExitStatus>> {
    // Spawn a background thread to wait() on the child so we don't block the
    // async executor.  The child's exit status is sent through a oneshot.
    let mut child_done: Option<tokio::sync::oneshot::Receiver<std::io::Result<ExitStatus>>> =
        if let Some(mut c) = child {
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(c.wait());
            });
            Some(rx)
        } else {
            None
        };

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut traces_captured: u64 = 0;
    let mut child_status: Option<ExitStatus> = None;

    let mut emit = |t: &HttpTrace| -> anyhow::Result<()> {
        store.insert(t).ok();
        println!("{}", serde_json::to_string(&TraceView::render(t, opts))?);
        traces_captured += 1;
        Ok(())
    };

    loop {
        tokio::select! {
            maybe_trace = trace_rx.recv() => {
                match maybe_trace {
                    Some(t) => emit(&t)?,
                    None => break,
                }
            }
            _ = &mut ctrl_c => break,
            // When the child exits, wait briefly for the backend to flush any
            // in-flight datagrams, then drain whatever arrived.
            status = async {
                if let Some(rx) = child_done.as_mut() {
                    rx.await
                } else {
                    std::future::pending().await
                }
            } => {
                if let Ok(Ok(status)) = status {
                    child_status = Some(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                while let Ok(t) = trace_rx.try_recv() {
                    emit(&t)?;
                }
                break;
            }
        }
    }

    if !quiet {
        // Machine-readable end-of-run summary on stderr (stdout stays pure JSONL).
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "exit",
                "child_exit_code": child_status.and_then(|s| s.code()),
                "traces_captured": traces_captured,
            })
        );
    }

    Ok(child_status)
}

pub async fn run_proxy(
    globals: &GlobalOpts,
    args: RunArgs,
    store: Arc<FjallTraceStore>,
) -> anyhow::Result<Option<ExitStatus>> {
    let fault_config = build_fault_config(&args.fault)?;
    let mut backend =
        ProxyCaptureBackend::new(args.bind, args.port, args.insecure).with_faults(fault_config);
    let backend_name = backend.name().to_string();
    let trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;

    // Always wait for the proxy to actually be listening — needed both to
    // safely inject env vars/CA into a spawned child (if any) and to export
    // the MITM CA certificate for out-of-process consumers (e.g. a Docker
    // sidecar target container). This also surfaces bind failures (e.g. port
    // already in use) as a clear error instead of a silent hang.
    let connect_ip = loopback_safe(args.bind);
    wait_for_proxy(connect_ip, args.port).await?;

    // Export the MITM CA certificate to a stable path so it can be trusted
    // by out-of-process consumers (bind-mount <data_dir>/ca.pem into another
    // container's trust store). Regenerated fresh every run.
    let ca_cert_pem = backend.ca_cert_pem();
    let ca_cert_path = globals.data_dir.join("ca.pem");
    match ca_cert_pem.as_deref() {
        Some(pem) => {
            std::fs::write(&ca_cert_path, pem).map_err(|e| {
                anyhow::anyhow!("failed to write CA cert to {}: {e}", ca_cert_path.display())
            })?;
        }
        None => {
            // Should not happen: ca_cert_pem is populated before the proxy
            // binds, and wait_for_proxy() above already confirmed the port
            // is accepting connections.
            warn!("MITM CA certificate unavailable after proxy became ready; ca.pem not written");
        }
    }

    // Optionally spawn a child command routed through the proxy.
    let child_and_script: Option<(std::process::Child, Option<TempScript>)> =
        if !args.command.is_empty() {
            let (child, ts) =
                spawn_proxy_child(&args.command, connect_ip, args.port, ca_cert_pem.as_deref())?;
            if !globals.quiet {
                eprintln!(
                    "phantom: spawned PID {} → {}",
                    child.id(),
                    args.command.join(" ")
                );
            }
            Some((child, ts))
        } else {
            None
        };

    let mut child_status = None;
    match args.output {
        OutputMode::Tui => {
            if !globals.quiet {
                if args.command.is_empty() {
                    if args.bind.is_unspecified() {
                        eprintln!(
                            "phantom: proxy listening on {}:{} (all interfaces)",
                            args.bind, args.port
                        );
                        eprintln!(
                            "  From this host:                http://127.0.0.1:{}",
                            args.port
                        );
                        eprintln!(
                            "  From other Docker containers:  http://<compose-service-name>:{}",
                            args.port
                        );
                    } else {
                        eprintln!("phantom: proxy listening on {}:{}", args.bind, args.port);
                        eprintln!(
                            "  Set your HTTP proxy to http://{}:{}",
                            args.bind, args.port
                        );
                        eprintln!(
                            "  Example: curl -x http://{}:{} http://httpbin.org/get",
                            args.bind, args.port
                        );
                    }
                    eprintln!("  MITM CA certificate: {}", ca_cert_path.display());
                    eprintln!("  Trust this CA in your target app/container for HTTPS capture.");
                }
                eprintln!("phantom: traces stored in {}", globals.data_dir.display());
            }
            phantom_tui::run_tui(store, trace_rx, &backend_name).await?;
        }
        OutputMode::Jsonl => {
            if !globals.quiet {
                eprintln!(
                    "phantom: proxy listening on {}:{} [jsonl mode]",
                    args.bind, args.port
                );
                eprintln!("  MITM CA certificate: {}", ca_cert_path.display());
            }
            // Split into child and script guard separately so the TempScript
            // is NOT dropped until after run_jsonl_output completes (the file
            // must exist while node is loading it via --require).
            let (child, _script_guard) = match child_and_script {
                Some((c, ts)) => (Some(c), ts),
                None => (None, None),
            };
            let opts = jsonl_render_options(&args);
            child_status = run_jsonl_output(store, trace_rx, child, &opts, globals.quiet).await?;
            // _script_guard dropped here — temp file deleted after child exits.
        }
    }

    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(child_status)
}

#[cfg(target_os = "linux")]
pub async fn run_ldpreload(
    globals: &GlobalOpts,
    args: RunArgs,
    store: Arc<FjallTraceStore>,
) -> anyhow::Result<Option<ExitStatus>> {
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
            Usage: phantom run --backend ldpreload --agent-lib ./libphantom_agent.so -- curl http://example.com"
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

    if !globals.quiet {
        eprintln!("phantom: ldpreload backend active");
        eprintln!("  agent lib : {}", agent_lib.display());
        eprintln!("  socket    : {}", socket_path.display());
        eprintln!("  command   : {}", args.command.join(" "));
        eprintln!("phantom: traces stored in {}", globals.data_dir.display());
    }

    // Spawn the target process with LD_PRELOAD and PHANTOM_SOCKET set.
    let child = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .env("LD_PRELOAD", &agent_lib)
        .env("PHANTOM_SOCKET", &socket_path)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {e}", args.command[0]))?;

    if !globals.quiet {
        eprintln!("phantom: spawned PID {}", child.id());
    }

    let mut child_status = None;
    match args.output {
        OutputMode::Tui => {
            // In TUI mode the user quits manually; child runs in background.
            phantom_tui::run_tui(store, trace_rx, &backend_name).await?;
        }
        OutputMode::Jsonl => {
            // In JSONL mode we exit automatically when the child finishes.
            let opts = jsonl_render_options(&args);
            child_status =
                run_jsonl_output(store, trace_rx, Some(child), &opts, globals.quiet).await?;
        }
    }

    backend.stop().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(child_status)
}
