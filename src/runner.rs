use std::path::{Path, PathBuf};

use phantom_capture::FaultConfig;

// ─────────────────────────────────────────────────────────────────────────────
// Embedded injection assets
// ─────────────────────────────────────────────────────────────────────────────

/// The proxy-preload.js content, embedded at compile time.
/// Written to a temp file when tracing Node.js processes via `phantom run -- node …`.
const NODE_PROXY_PRELOAD: &str = include_str!("../tests/apps/node-app/proxy-preload.js");

/// The Java Agent JAR, embedded at compile time.
/// Written to a temp file when tracing Java processes via `phantom run -- java …`.
const JAVA_AGENT_JAR: &[u8] = include_bytes!("../crates/phantom-java-agent/phantom-java-agent.jar");

// ─────────────────────────────────────────────────────────────────────────────
// Child process spawning
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard that deletes a temporary script file on drop.
pub struct TempScript(pub PathBuf);

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

/// Returns `true` if `exe` (path or bare name) resolves to `php` or a
/// version-suffixed PHP binary (e.g. `php7.4`, `php8.2`, `php5.3`).
fn is_php_command(exe: &str) -> bool {
    let base = Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(exe);
    base == "php"
        || (base.starts_with("php")
            && !base[3..].is_empty()
            && base[3..].chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Spawns `command` as a child process routed through the phantom proxy.
///
/// * `HTTP_PROXY` / `HTTPS_PROXY` (and lowercase variants) are set so plain
///   HTTP and HTTP-client-honoured HTTPS (e.g. libcurl) are captured.
/// * For Node.js executables the embedded proxy-preload script is written to a
///   temp file and prepended as `--require <path>` so HTTPS is also captured
///   without touching the application source.
/// * For Java executables, the phantom-java-agent.jar is injected via -javaagent
///   to force proxy settings and bypass SSL verification globally.
/// * For PHP executables, the MITM CA certificate is written to a temp PEM
///   file and injected via `-d curl.cainfo=<path>` so the curl extension
///   trusts phantom's HTTPS interception without any application changes.
///
/// Returns `(child, Option<TempScript>)`.  The `TempScript` must be kept alive
/// until after the child exits so the file is not deleted prematurely.
pub fn spawn_proxy_child(
    command: &[String],
    proxy_port: u16,
    ca_cert_pem: Option<&str>,
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

    if is_php_command(exe)
        && let Some(pem) = ca_cert_pem
    {
        // Write the MITM CA certificate to a temp PEM file.
        let ca_path = std::env::temp_dir().join(format!("phantom-ca-{}.pem", std::process::id()));
        std::fs::write(&ca_path, pem)
            .map_err(|e| anyhow::anyhow!("failed to write CA cert: {e}"))?;
        temp_script = Some(TempScript(ca_path.clone()));

        // Prepend -d curl.cainfo=<path> before the rest of the args, so
        // the curl extension trusts phantom's MITM certificate without
        // any changes to the application's own curl_setopt() calls.
        let mut args = vec![
            "-d".to_string(),
            format!("curl.cainfo={}", ca_path.display()),
        ];
        args.extend_from_slice(&actual_args);
        actual_args = args;
    }

    let mut cmd = std::process::Command::new(exe);
    cmd.args(&actual_args)
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url);

    // Node.js handles HTTPS itself via the injected proxy-preload.js (a
    // custom ProxyTunnelAgent / undici ProxyAgent). Setting HTTPS_PROXY here
    // would make libraries like axios configure their own competing
    // httpsAgent from the env var, conflicting with that injected agent and
    // breaking HTTPS capture. So HTTPS_PROXY/NO_PROXY are only set for
    // non-Node commands (e.g. curl, PHP's curl extension), which rely on
    // libcurl's native env-var proxy detection instead.
    if !is_node_command(exe) {
        cmd.env("HTTPS_PROXY", &proxy_url)
            .env("https_proxy", &proxy_url)
            // Clear any inherited no-proxy exclusions (e.g. for `localhost`)
            // so libcurl doesn't bypass phantom's proxy for local targets.
            // Mirrors the Java branch's explicit `-Dhttp.nonProxyHosts=` below.
            .env("NO_PROXY", "")
            .env("no_proxy", "");
    }

    // For Java processes, inject proxy settings and the Java Agent via JAVA_TOOL_OPTIONS.
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

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {:?}: {e}", exe))?;

    Ok((child, temp_script))
}

/// Poll until the proxy port is accepting connections (or timeout after 5 s).
pub async fn wait_for_proxy(port: u16) -> anyhow::Result<()> {
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

/// Parse repeated `--fault SPEC` flags into a `FaultConfig`.
pub fn build_fault_config(specs: &[String]) -> anyhow::Result<FaultConfig> {
    let mut rules = Vec::new();
    for spec in specs {
        let rule = phantom_capture::parse_fault_spec(spec)
            .map_err(|e| anyhow::anyhow!("--fault {spec:?}: {e}"))?;
        rules.push(rule);
    }
    Ok(FaultConfig { rules })
}
