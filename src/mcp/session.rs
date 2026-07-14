use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use phantom_capture::ProxyCaptureBackend;
use phantom_core::capture::CaptureBackend;
use phantom_core::storage::TraceStore;
use phantom_storage::FjallTraceStore;

use crate::runner::{TempScript, build_fault_config, spawn_proxy_child, wait_for_proxy};

/// Lifecycle state of a capture session's traced child process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildState {
    /// No child was spawned (proxy-only session).
    None,
    Running,
    Exited(Option<i32>),
}

/// One running capture: a MITM proxy plus (optionally) a traced child process.
pub struct CaptureSession {
    pub id: String,
    pub port: u16,
    pub command: Vec<String>,
    pub child_pid: Option<u32>,
    pub started_at_ms: u64,
    pub trace_count: Arc<AtomicU64>,
    child_state: Arc<Mutex<ChildState>>,
    child: Option<Arc<Mutex<std::process::Child>>>,
    backend: ProxyCaptureBackend,
    /// Keeps the injected preload/CA temp file alive for the child's lifetime.
    _temp_script: Option<TempScript>,
}

impl CaptureSession {
    pub fn child_state(&self) -> ChildState {
        *self.child_state.lock().unwrap()
    }
}

/// Owns all capture sessions of one MCP server process.
#[derive(Default)]
pub struct CaptureManager {
    sessions: Mutex<HashMap<String, CaptureSession>>,
}

/// Snapshot of a session for `capture_status` responses.
pub struct SessionStatus {
    pub id: String,
    pub port: u16,
    pub command: Vec<String>,
    pub child_pid: Option<u32>,
    pub started_at_ms: u64,
    pub trace_count: u64,
    pub child_state: ChildState,
}

fn random_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:08x}", (nanos as u64 ^ (nanos >> 64) as u64) as u32)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Reserve an OS-assigned free TCP port (small race until the proxy binds it).
fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

impl CaptureManager {
    /// Starts a proxy capture session, optionally spawning `command` through it.
    /// Traces are pumped into `store`; the session runs until `stop()`.
    pub async fn start(
        &self,
        store: Arc<FjallTraceStore>,
        command: Vec<String>,
        port: Option<u16>,
        insecure: bool,
        fault: &[String],
    ) -> anyhow::Result<SessionStatus> {
        let port = match port {
            Some(p) => p,
            None => free_port()?,
        };
        // MCP sessions are always local (stdio-driven by an AI coding agent on
        // the same host), so the proxy only ever binds loopback here — there
        // is no --bind flag in this mode.
        let bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let fault_config = build_fault_config(fault)?;
        let mut backend =
            ProxyCaptureBackend::new(bind_ip, port, insecure).with_faults(fault_config);
        let mut trace_rx = backend.start().map_err(|e| anyhow::anyhow!("{e}"))?;
        wait_for_proxy(bind_ip, port).await?;

        // Pump captured traces into the store off the async executor.
        let trace_count = Arc::new(AtomicU64::new(0));
        let pump_count = trace_count.clone();
        let pump_store = store.clone();
        tokio::spawn(async move {
            while let Some(trace) = trace_rx.recv().await {
                let store = pump_store.clone();
                let insert = tokio::task::spawn_blocking(move || store.insert(&trace)).await;
                match insert {
                    Ok(Ok(())) => {
                        pump_count.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(Err(e)) => tracing::warn!("failed to store trace: {e}"),
                    Err(e) => tracing::warn!("trace insert task failed: {e}"),
                }
            }
        });

        // Optionally spawn the traced child.
        let child_state = Arc::new(Mutex::new(ChildState::None));
        let mut child_pid = None;
        let mut child_handle = None;
        let mut temp_script = None;
        if !command.is_empty() {
            let ca_cert_pem = backend.ca_cert_pem();
            let (child, ts) = spawn_proxy_child(&command, bind_ip, port, ca_cert_pem.as_deref())?;
            child_pid = Some(child.id());
            temp_script = ts;
            *child_state.lock().unwrap() = ChildState::Running;

            let child = Arc::new(Mutex::new(child));
            child_handle = Some(child.clone());
            let state = child_state.clone();
            // Poll try_wait instead of a blocking wait() so stop() can take
            // the child lock to kill without deadlocking against the waiter.
            tokio::spawn(async move {
                loop {
                    let status = child.lock().unwrap().try_wait();
                    match status {
                        Ok(Some(st)) => {
                            *state.lock().unwrap() = ChildState::Exited(st.code());
                            break;
                        }
                        Ok(None) => {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        let session = CaptureSession {
            id: random_session_id(),
            port,
            command,
            child_pid,
            started_at_ms: now_ms(),
            trace_count,
            child_state,
            child: child_handle,
            backend,
            _temp_script: temp_script,
        };
        let status = snapshot(&session);
        self.sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), session);
        Ok(status)
    }

    /// Stops a session: kills a still-running child and shuts the proxy down.
    /// Returns the final status, or `None` for an unknown session ID.
    pub fn stop(&self, session_id: &str) -> Option<SessionStatus> {
        let mut session = self.sessions.lock().unwrap().remove(session_id)?;
        shutdown_session(&mut session);
        Some(snapshot(&session))
    }

    /// Status of one session, or all sessions when `session_id` is `None`.
    pub fn status(&self, session_id: Option<&str>) -> Vec<SessionStatus> {
        let sessions = self.sessions.lock().unwrap();
        match session_id {
            Some(id) => sessions.get(id).map(snapshot).into_iter().collect(),
            None => {
                let mut all: Vec<_> = sessions.values().map(snapshot).collect();
                all.sort_by_key(|s| s.started_at_ms);
                all
            }
        }
    }

    pub fn active_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// Stops every session (server shutdown path).
    pub fn stop_all(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        for (_, mut session) in sessions.drain() {
            shutdown_session(&mut session);
        }
    }
}

fn snapshot(session: &CaptureSession) -> SessionStatus {
    SessionStatus {
        id: session.id.clone(),
        port: session.port,
        command: session.command.clone(),
        child_pid: session.child_pid,
        started_at_ms: session.started_at_ms,
        trace_count: session.trace_count.load(Ordering::Relaxed),
        child_state: session.child_state(),
    }
}

fn shutdown_session(session: &mut CaptureSession) {
    if let Some(child) = &session.child
        && session.child_state() == ChildState::Running
    {
        let mut child = child.lock().unwrap();
        let _ = child.kill();
        if let Ok(st) = child.wait() {
            *session.child_state.lock().unwrap() = ChildState::Exited(st.code());
        }
    }
    if let Err(e) = session.backend.stop() {
        tracing::warn!("failed to stop capture backend: {e}");
    }
}
