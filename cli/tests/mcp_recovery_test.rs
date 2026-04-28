//! End-to-end MCP bridge recovery tests.
//!
//! These tests use the release binary because they exercise real process
//! boundaries: a headless hub process and a separate `mcp-serve` process.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn get_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.pop();
    path.push("release");
    path.push("botster");
    path
}

fn binary_exists() -> bool {
    get_binary_path().exists()
}

struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child }
    }

    fn kill_and_wait(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill_and_wait();
        }
    }
}

struct JsonLineClient {
    process: ChildGuard,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
}

impl JsonLineClient {
    fn spawn(config_dir: &Path, session_uuid: &str) -> Self {
        let mut child = Command::new(get_binary_path())
            .arg("mcp-serve")
            .env("BOTSTER_ENV", "test")
            .env("BOTSTER_OFFLINE", "1")
            .env("BOTSTER_CONFIG_DIR", config_dir)
            .env("BOTSTER_SESSION_UUID", session_uuid)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn botster mcp-serve");

        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = child.stdout.take().expect("mcp stdout");
        let stderr = child.stderr.take().expect("mcp stderr");

        let (line_tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = line_tx.send(line);
            }
        });

        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = String::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                buf.push_str(&line);
                buf.push('\n');
                let _ = stderr_tx.send(buf.clone());
            }
        });

        Self {
            process: ChildGuard::new(child),
            stdin,
            lines,
            stderr: stderr_rx,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").expect("write JSON-RPC message");
        self.stdin.flush().expect("flush JSON-RPC message");
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.response(id, Duration::from_secs(10))
    }

    fn response(&mut self, id: u64, timeout: Duration) -> Value {
        let start = Instant::now();
        loop {
            let remaining = timeout
                .checked_sub(start.elapsed())
                .unwrap_or_else(|| Duration::from_millis(0));
            let line = self
                .lines
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for JSON-RPC response id={id}"));
            let value: Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad JSON line {line}: {e}"));
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    fn stderr_so_far(&self) -> String {
        let mut latest = String::new();
        while let Ok(chunk) = self.stderr.try_recv() {
            latest = chunk;
        }
        latest
    }
}

fn wait_for_hub_ready(child: &mut Child, timeout: Duration) {
    let stdout = child.stdout.take().expect("hub stdout");
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("Hub ready") {
                let _ = ready_tx.send(());
            }
        }
    });

    ready_rx
        .recv_timeout(timeout)
        .expect("headless hub did not become ready");
}

fn read_hub_manifest(config_dir: &Path) -> Value {
    let start = Instant::now();
    loop {
        let hubs_dir = config_dir.join("hubs");
        if let Ok(entries) = std::fs::read_dir(&hubs_dir) {
            for entry in entries.flatten() {
                let manifest_path = entry.path().join("manifest.json");
                if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                    if let Ok(manifest) = serde_json::from_str::<Value>(&content) {
                        return manifest;
                    }
                }
            }
        }

        if start.elapsed() > Duration::from_secs(5) {
            panic!("hub manifest did not appear under {}", hubs_dir.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn write_test_session_manifest(config_dir: &Path, session_uuid: &str, hub_manifest_path: &Path) {
    for session_dir in [
        config_dir
            .join("workspaces")
            .join("ws-mcp-recovery")
            .join("sessions")
            .join(session_uuid),
        config_dir
            .join("workspaces")
            .join("ws-mcp-recovery")
            .join("sessions")
            .join("sessions")
            .join(session_uuid),
    ] {
        std::fs::create_dir_all(&session_dir).expect("create fake session dir");
        std::fs::write(
            session_dir.join("manifest.json"),
            json!({
                "session_uuid": session_uuid,
                "hub_manifest_path": hub_manifest_path,
                "workspace_id": "ws-mcp-recovery",
                "agent_name": "test-agent",
            })
            .to_string(),
        )
        .expect("write fake session manifest");
    }
}

fn wait_for_socket_path(path: &Path, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("socket path was not repaired: {}", path.display());
}

#[test]
#[cfg(unix)]
fn mcp_serve_recovers_when_started_after_hub_socket_path_is_deleted() {
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().expect("temp config dir");
    let config_dir = temp_dir.path();
    let mut hub = ChildGuard::new(
        Command::new(get_binary_path())
            .arg("start")
            .arg("--headless")
            .arg("--offline")
            .env("BOTSTER_ENV", "test")
            .env("BOTSTER_OFFLINE", "1")
            .env("BOTSTER_CONFIG_DIR", config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn headless hub"),
    );

    wait_for_hub_ready(&mut hub.child, Duration::from_secs(20));
    let manifest = read_hub_manifest(config_dir);
    let socket_path = PathBuf::from(
        manifest
            .get("socket_path")
            .and_then(Value::as_str)
            .expect("hub manifest socket_path"),
    );
    let hub_manifest_path = config_dir
        .join("hubs")
        .join(manifest["hub_id"].as_str().expect("hub_id"))
        .join("manifest.json");

    std::fs::remove_file(&socket_path).expect("delete public hub socket path");
    assert!(
        !socket_path.exists(),
        "test must start mcp-serve while hub socket path is missing"
    );

    let session_uuid = "sess-mcp-recovery-0001";
    write_test_session_manifest(config_dir, session_uuid, &hub_manifest_path);

    let mut mcp = JsonLineClient::spawn(config_dir, session_uuid);
    let init = mcp.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "botster-recovery-test", "version": "0.0.0" }
        }),
    );
    assert!(
        init.get("result").is_some(),
        "mcp-serve should initialize while retrying; response={init}, stderr={}",
        mcp.stderr_so_far()
    );
    mcp.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));

    wait_for_socket_path(&socket_path, Duration::from_secs(12));

    let start = Instant::now();
    loop {
        let response = mcp.request(
            2 + start.elapsed().as_millis() as u64,
            "tools/list",
            json!({}),
        );
        let tools = response
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let names = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();

        if names.contains(&"list_plugins") {
            break;
        }

        assert!(
            start.elapsed() < Duration::from_secs(10),
            "mcp-serve did not expose real hub tools after socket repair; response={response}, stderr={}",
            mcp.stderr_so_far()
        );
        thread::sleep(Duration::from_millis(250));
    }

    mcp.process.kill_and_wait();
    hub.kill_and_wait();
}
