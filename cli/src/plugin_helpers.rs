//! Generic plugin command resolution and URL readiness helpers.
//!
//! Plugins sometimes discover a public URL before the hostname is globally
//! resolvable. The URL is only considered ready once public DNS returns an
//! address and the HTTPS origin itself responds.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const PUBLIC_DOH_URL: &str = "https://1.1.1.1/dns-query";
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const COMMAND_GATE_CAPTURE_LIMIT_BYTES: usize = 8 * 1024;
const COMMAND_GATE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const COMMAND_GATE_TERM_GRACE: Duration = Duration::from_millis(300);

/// Prepared command returned after any blocking plugin setup completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPluginCommand {
    /// Absolute executable path resolved from the Hub process environment.
    pub command: PathBuf,
    /// Optional config file path written for the plugin command.
    pub config_path: Option<PathBuf>,
}

/// Structured reason for plugin command preparation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommandPrepareErrorKind {
    /// Command was empty after trimming.
    CommandBlank,
    /// Command could not be found or was not executable.
    CommandMissing,
    /// Optional config file could not be written.
    ConfigWriteFailed,
}

impl PluginCommandPrepareErrorKind {
    /// Stable string exposed to Lua plugins.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandBlank => "command_blank",
            Self::CommandMissing => "command_missing",
            Self::ConfigWriteFailed => "config_write_failed",
        }
    }
}

/// Error returned by plugin command preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommandPrepareError {
    /// Stable machine-readable failure kind.
    pub kind: PluginCommandPrepareErrorKind,
    /// User/log-facing failure message.
    pub message: String,
}

impl PluginCommandPrepareError {
    fn new(kind: PluginCommandPrepareErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PluginCommandPrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginCommandPrepareError {}

/// Input for a one-shot plugin command gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGateRequest {
    /// Command line to run. The first shell word is resolved through
    /// `prepare_plugin_command`; remaining words become argv.
    pub command: String,
    /// Required working directory.
    pub cwd: PathBuf,
    /// Deadline for the child process.
    pub timeout: Duration,
    /// Environment variables to add/override for the command.
    pub env: BTreeMap<String, String>,
    /// Optional config file path to write before the command runs.
    pub config_path: Option<PathBuf>,
    /// Optional config file contents.
    pub config_contents: Option<String>,
}

/// Bounded command output captured for a command gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGateOutputSummary {
    /// Captured stdout tail, lossy UTF-8 decoded.
    pub stdout_tail: String,
    /// Captured stderr tail, lossy UTF-8 decoded.
    pub stderr_tail: String,
    /// True when either stream exceeded its capture bound.
    pub truncated: bool,
}

/// Completed one-shot command gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGateCompletion {
    /// Whether the command completed before timeout with exit status 0.
    pub success: bool,
    /// Process exit status when available.
    pub exit_status: Option<i32>,
    /// Bounded output summary.
    pub output_summary: CommandGateOutputSummary,
    /// Stable machine-readable error kind.
    pub error_kind: Option<String>,
    /// Human-readable error message.
    pub error: Option<String>,
    /// Elapsed runtime in milliseconds.
    pub duration_ms: u128,
}

#[derive(Debug, Default)]
struct CapturedPipe {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Run a plugin command gate synchronously.
///
/// This helper performs blocking process I/O and must be called from
/// `spawn_blocking` or another non-hub thread.
pub fn run_command_gate(request: CommandGateRequest) -> CommandGateCompletion {
    let started = Instant::now();
    let trimmed = request.command.trim();
    if trimmed.is_empty() {
        return command_gate_validation_error(started, "command_blank", "Command cannot be blank");
    }
    if request.cwd.as_os_str().is_empty() {
        return command_gate_validation_error(started, "cwd_missing", "cwd is required");
    }
    if !request.cwd.is_dir() {
        return command_gate_validation_error(
            started,
            "cwd_invalid",
            format!("cwd is not a directory: {}", request.cwd.display()),
        );
    }
    if request.timeout.is_zero() {
        return command_gate_validation_error(
            started,
            "timeout_invalid",
            "timeout must be greater than zero",
        );
    }

    let args = match split_command_words(trimmed) {
        Ok(args) if !args.is_empty() => args,
        Ok(_) => {
            return command_gate_validation_error(
                started,
                "command_blank",
                "Command cannot be blank",
            );
        }
        Err(e) => {
            return command_gate_validation_error(
                started,
                "command_parse_failed",
                format!("Failed to parse command: {e}"),
            );
        }
    };
    let Some((program, program_args)) = args.split_first() else {
        return command_gate_validation_error(started, "command_blank", "Command cannot be blank");
    };
    let prepared = match prepare_plugin_command(
        program,
        request.config_path.as_deref(),
        request.config_contents.as_deref(),
    ) {
        Ok(prepared) => prepared,
        Err(e) => {
            return command_gate_validation_error(started, e.kind.as_str(), e.to_string());
        }
    };

    let stdout_capture = Arc::new(Mutex::new(CapturedPipe::default()));
    let stderr_capture = Arc::new(Mutex::new(CapturedPipe::default()));
    let mut command = Command::new(&prepared.command);
    command
        .args(program_args)
        .current_dir(&request.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in request.env {
        command.env(key, value);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs in the child process after fork and before
        // exec. It only calls async-signal-safe setpgid and constructs an OS
        // error from errno on failure.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return command_gate_validation_error(
                started,
                "spawn_failed",
                format!("Failed to spawn command gate: {e}"),
            );
        }
    };

    let stdout_thread = child.stdout.take().map(|stdout| {
        spawn_capture_thread(
            stdout,
            Arc::clone(&stdout_capture),
            COMMAND_GATE_CAPTURE_LIMIT_BYTES,
        )
    });
    let stderr_thread = child.stderr.take().map(|stderr| {
        spawn_capture_thread(
            stderr,
            Arc::clone(&stderr_capture),
            COMMAND_GATE_CAPTURE_LIMIT_BYTES,
        )
    });

    let deadline = started + request.timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    terminate_command_gate_child(&mut child);
                    break child.wait().ok();
                }
                thread::sleep(COMMAND_GATE_POLL_INTERVAL);
            }
            Err(e) => {
                terminate_command_gate_child(&mut child);
                join_capture_thread(stdout_thread);
                join_capture_thread(stderr_thread);
                let output_summary = collect_command_gate_output(&stdout_capture, &stderr_capture);
                return CommandGateCompletion {
                    success: false,
                    exit_status: None,
                    output_summary,
                    error_kind: Some("wait_failed".to_string()),
                    error: Some(format!("Failed while waiting for command gate: {e}")),
                    duration_ms: started.elapsed().as_millis(),
                };
            }
        }
    };

    join_capture_thread(stdout_thread);
    join_capture_thread(stderr_thread);
    let output_summary = collect_command_gate_output(&stdout_capture, &stderr_capture);
    let exit_status = status.as_ref().and_then(std::process::ExitStatus::code);
    let success = !timed_out
        && status
            .as_ref()
            .is_some_and(std::process::ExitStatus::success);
    let (error_kind, error) = if timed_out {
        (
            Some("timeout".to_string()),
            Some(format!(
                "Command timed out after {:.3}s",
                request.timeout.as_secs_f64()
            )),
        )
    } else if success {
        (None, None)
    } else {
        (
            Some("exit_status".to_string()),
            Some(match exit_status {
                Some(code) => format!("Command exited with status {code}"),
                None => "Command exited without an exit status".to_string(),
            }),
        )
    };

    CommandGateCompletion {
        success,
        exit_status,
        output_summary,
        error_kind,
        error,
        duration_ms: started.elapsed().as_millis(),
    }
}

fn split_command_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (None, '\'' | '"') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (Some('\''), '\\') => current.push('\\'),
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push('\\');
                }
            }
            (_, c) => current.push(c),
        }
    }

    if let Some(q) = quote {
        return Err(format!("unterminated {q} quote"));
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

fn command_gate_validation_error(
    started: Instant,
    kind: impl Into<String>,
    message: impl Into<String>,
) -> CommandGateCompletion {
    CommandGateCompletion {
        success: false,
        exit_status: None,
        output_summary: CommandGateOutputSummary {
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            truncated: false,
        },
        error_kind: Some(kind.into()),
        error: Some(message.into()),
        duration_ms: started.elapsed().as_millis(),
    }
}

fn spawn_capture_thread<R>(
    mut reader: R,
    capture: Arc<Mutex<CapturedPipe>>,
    limit: usize,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = [0_u8; 1024];
        loop {
            let Ok(read) = reader.read(&mut buf) else {
                break;
            };
            if read == 0 {
                break;
            }
            if let Ok(mut captured) = capture.lock() {
                captured.bytes.extend_from_slice(&buf[..read]);
                if captured.bytes.len() > limit {
                    let overflow = captured.bytes.len() - limit;
                    captured.bytes.drain(..overflow);
                    captured.truncated = true;
                }
            }
        }
    })
}

fn join_capture_thread(handle: Option<thread::JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

fn collect_command_gate_output(
    stdout_capture: &Arc<Mutex<CapturedPipe>>,
    stderr_capture: &Arc<Mutex<CapturedPipe>>,
) -> CommandGateOutputSummary {
    let stdout = snapshot_captured_pipe(stdout_capture);
    let stderr = snapshot_captured_pipe(stderr_capture);
    CommandGateOutputSummary {
        stdout_tail: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stderr_tail: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        truncated: stdout.truncated || stderr.truncated,
    }
}

fn snapshot_captured_pipe(capture: &Arc<Mutex<CapturedPipe>>) -> CapturedPipe {
    capture
        .lock()
        .map(|guard| CapturedPipe {
            bytes: guard.bytes.clone(),
            truncated: guard.truncated,
        })
        .unwrap_or_default()
}

fn terminate_command_gate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let Ok(pgid) = libc::pid_t::try_from(child.id()) else {
            let _ = child.kill();
            return;
        };
        // The process may have exited between try_wait and timeout cleanup.
        // SAFETY: killpg is called with the child process group created by
        // pre_exec above. Errors are intentionally ignored because the child
        // may already have exited.
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
        thread::sleep(COMMAND_GATE_TERM_GRACE);
        if matches!(child.try_wait(), Ok(None)) {
            // SAFETY: same process-group cleanup as the SIGTERM above.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

/// Resolve a command using the current process environment.
///
/// Returns an absolute path when the command exists and is executable.
#[must_use]
pub fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    if looks_like_path(trimmed) {
        return resolve_candidate(Path::new(trimmed));
    }

    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(trimmed);
        if let Some(resolved) = resolve_candidate(&candidate) {
            return Some(resolved);
        }
    }

    None
}

/// Resolve a plugin command and optionally write a config file.
///
/// This helper is intentionally synchronous so callers can run it inside
/// `spawn_blocking` while keeping Lua action handlers responsive.
pub fn prepare_plugin_command(
    command: &str,
    config_path: Option<&Path>,
    config_contents: Option<&str>,
) -> Result<PreparedPluginCommand, PluginCommandPrepareError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(PluginCommandPrepareError::new(
            PluginCommandPrepareErrorKind::CommandBlank,
            "Command cannot be blank",
        ));
    }

    let resolved = resolve_command_path(trimmed).ok_or_else(|| {
        PluginCommandPrepareError::new(
            PluginCommandPrepareErrorKind::CommandMissing,
            format!("Command not found: {trimmed}"),
        )
    })?;

    let prepared_config_path = if let Some(path) = config_path {
        if let Some(contents) = config_contents {
            std::fs::write(path, contents).map_err(|e| {
                PluginCommandPrepareError::new(
                    PluginCommandPrepareErrorKind::ConfigWriteFailed,
                    format!(
                        "Failed to write plugin command config {}: {e}",
                        path.display()
                    ),
                )
            })?;
        }
        Some(path.to_path_buf())
    } else {
        None
    };

    Ok(PreparedPluginCommand {
        command: resolved,
        config_path: prepared_config_path,
    })
}

fn looks_like_path(command: &str) -> bool {
    Path::new(command).is_absolute() || command.contains('/') || command.contains('\\')
}

fn resolve_candidate(path: &Path) -> Option<PathBuf> {
    if !is_executable_file(path) {
        return None;
    }

    Some(
        std::fs::canonicalize(path)
            .ok()
            .unwrap_or_else(|| path.to_path_buf()),
    )
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Poll public DNS plus the preview HTTPS origin until both are ready.
pub async fn wait_until_url_ready(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    wait_until_url_ready_with_doh_url(hostname, url, timeout, PUBLIC_DOH_URL).await
}

async fn wait_until_url_ready_with_doh_url(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
    doh_url: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build URL probe client: {e}"))?;

    let doh_query = format!("{doh_url}?name={hostname}&type=A");
    let mut last_error = "preview never became reachable".to_string();

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }

        match dns_has_a_record(&client, &doh_query).await {
            Ok(true) => {}
            Ok(false) => {
                last_error = "dns returned NOERROR but no A records".to_string();
                tokio::time::sleep(PROBE_INTERVAL).await;
                continue;
            }
            Err(e) => {
                last_error = e;
                tokio::time::sleep(PROBE_INTERVAL).await;
                continue;
            }
        }

        match client.get(url).send().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_error = format!("HTTPS probe failed: {e}");
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        }
    }
}

async fn dns_has_a_record(client: &reqwest::Client, doh_url: &str) -> Result<bool, String> {
    let response = client
        .get(doh_url)
        .header("Accept", "application/dns-json")
        .send()
        .await
        .map_err(|e| format!("DNS probe failed: {e}"))?;

    let json = response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("DNS response parse failed: {e}"))?;

    let status = json
        .get("Status")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(99);
    if status != 0 {
        return Err(format!("DNS returned rcode {status}"));
    }

    let has_a = json
        .get("Answer")
        .and_then(|v| v.as_array())
        .is_some_and(|answers| {
            answers
                .iter()
                .any(|answer| answer.get("type").and_then(serde_json::Value::as_u64) == Some(1))
        });
    Ok(has_a)
}

#[cfg(test)]
mod tests {
    use super::{
        prepare_plugin_command, resolve_command_path, run_command_gate, wait_until_url_ready,
        CommandGateRequest,
    };
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn resolves_direct_executable_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-connector");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&path);

        let resolved = resolve_command_path(path.to_str().unwrap());
        let expected = std::fs::canonicalize(&path).unwrap();
        assert_eq!(resolved.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn finds_executable_on_path() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-connector");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&path);

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());

        let resolved = resolve_command_path("preview-connector");

        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }

        let expected = std::fs::canonicalize(&path).unwrap();
        assert_eq!(resolved.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn returns_none_for_missing_command() {
        let _guard = env_lock().lock().unwrap();
        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", "/definitely/missing");

        let resolved = resolve_command_path("preview-connector");

        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }

        assert!(resolved.is_none());
    }

    #[test]
    fn prepares_command_and_writes_config() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("preview-connector");
        let config = dir.path().join("preview.json");
        std::fs::write(&command, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&command);

        let prepared =
            prepare_plugin_command(command.to_str().unwrap(), Some(&config), Some("{}\n"))
                .expect("command should prepare");

        assert_eq!(
            prepared.command,
            std::fs::canonicalize(&command).expect("canonical command")
        );
        assert_eq!(prepared.config_path.as_deref(), Some(config.as_path()));
        assert_eq!(
            std::fs::read_to_string(&config).expect("config should be written"),
            "{}\n"
        );
    }

    #[test]
    fn rejects_blank_prepared_command() {
        let err = prepare_plugin_command("  ", None, None).expect_err("blank command should fail");
        assert_eq!(err.kind, super::PluginCommandPrepareErrorKind::CommandBlank);
        assert_eq!(err.message, "Command cannot be blank");
    }

    #[test]
    fn command_gate_runs_command_with_cwd_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = BTreeMap::new();
        env.insert("BOTSTER_GATE_TEST".to_string(), "ok".to_string());

        let result = run_command_gate(CommandGateRequest {
            command: "/bin/sh -lc 'printf \"%s:%s\" \"$PWD\" \"$BOTSTER_GATE_TEST\"'".to_string(),
            cwd: dir.path().to_path_buf(),
            timeout: Duration::from_secs(2),
            env,
            config_path: None,
            config_contents: None,
        });

        assert!(result.success, "{result:?}");
        assert_eq!(result.exit_status, Some(0));
        let expected_cwd = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            result.output_summary.stdout_tail,
            format!("{}:ok", expected_cwd.display())
        );
        assert_eq!(result.error_kind, None);
    }

    #[test]
    fn command_gate_rejects_blank_command() {
        let result = run_command_gate(CommandGateRequest {
            command: "  ".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(1),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(!result.success);
        assert_eq!(result.error_kind.as_deref(), Some("command_blank"));
    }

    #[test]
    fn command_gate_rejects_missing_cwd() {
        let result = run_command_gate(CommandGateRequest {
            command: "true".to_string(),
            cwd: std::path::PathBuf::new(),
            timeout: Duration::from_secs(1),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(!result.success);
        assert_eq!(result.error_kind.as_deref(), Some("cwd_missing"));
    }

    #[test]
    fn command_gate_rejects_parse_error() {
        let result = run_command_gate(CommandGateRequest {
            command: "/bin/sh -lc 'unterminated".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(1),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(!result.success);
        assert_eq!(result.error_kind.as_deref(), Some("command_parse_failed"));
    }

    #[test]
    fn command_gate_single_quotes_keep_backslashes_literal() {
        let result = run_command_gate(CommandGateRequest {
            command: "/usr/bin/printf %s 'a\\b'".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(1),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(result.success, "{result:?}");
        assert_eq!(result.output_summary.stdout_tail, "a\\b");
    }

    #[test]
    fn command_gate_reports_config_write_failure() {
        let result = run_command_gate(CommandGateRequest {
            command: "/usr/bin/true".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(1),
            env: BTreeMap::new(),
            config_path: Some(std::env::temp_dir()),
            config_contents: Some("{}\n".to_string()),
        });

        assert!(!result.success);
        assert_eq!(result.error_kind.as_deref(), Some("config_write_failed"));
    }

    #[test]
    fn command_gate_times_out() {
        let result = run_command_gate(CommandGateRequest {
            command: "/bin/sh -lc 'sleep 2'".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_millis(100),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(!result.success);
        assert_eq!(result.error_kind.as_deref(), Some("timeout"));
    }

    #[test]
    fn command_gate_bounds_captured_output() {
        let result = run_command_gate(CommandGateRequest {
            command: "/bin/sh -lc 'yes x | head -n 20000'".to_string(),
            cwd: std::env::temp_dir(),
            timeout: Duration::from_secs(2),
            env: BTreeMap::new(),
            config_path: None,
            config_contents: None,
        });

        assert!(result.success, "{result:?}");
        assert_eq!(
            result.output_summary.stdout_tail.len(),
            super::COMMAND_GATE_CAPTURE_LIMIT_BYTES
        );
        assert!(result.output_summary.truncated);
    }

    #[tokio::test]
    async fn rejects_bogus_hostname_quickly() {
        let result = wait_until_url_ready(
            "this-hostname-will-never-exist.invalid",
            "https://this-hostname-will-never-exist.invalid",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(result.is_err());
    }
}
