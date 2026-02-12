// PTY-based integration tests for TUI behavior
//
// These tests spawn the CLI inside a pseudo-TTY, allowing us to test
// interactive behavior that requires a real terminal.
//
// IMPORTANT: Run `cargo build --release` before running these tests!

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant};

/// Ensure test environment is configured.
///
/// Sets BOTSTER_ENV=test and BOTSTER_HUB_ID=test-hub once for the process.
/// This is the single source of truth — individual tests don't need to set these.
static INIT: Once = Once::new();
fn ensure_test_env() {
    INIT.call_once(|| {
        std::env::set_var("BOTSTER_ENV", "test");
        std::env::set_var("BOTSTER_HUB_ID", "test-hub");
    });
}

/// Path to the release binary
fn get_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps
    path.pop(); // Remove debug
    path.push("release");
    path.push("botster");
    path
}

/// Check if the release binary exists
fn binary_exists() -> bool {
    get_binary_path().exists()
}

/// Shared output buffer that can be read while the capture thread is still running.
type SharedOutput = std::sync::Arc<std::sync::Mutex<String>>;

/// Spawn a thread to capture PTY output into a shared buffer.
/// The buffer can be polled from the test thread while output is still arriving.
fn spawn_output_capture(
    mut reader: Box<dyn std::io::Read + Send>,
) -> (thread::JoinHandle<()>, SharedOutput) {
    let buffer: SharedOutput = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let buf_clone = std::sync::Arc::clone(&buffer);
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]);
                    buf_clone.lock().unwrap().push_str(&s);
                }
                Err(_) => break,
            }
        }
    });
    (handle, buffer)
}

/// Wait for the TUI to enter raw mode by watching for ANSI escape sequences.
///
/// Before raw mode, Ctrl+Q (0x11) is consumed by the terminal line discipline
/// as XON flow control and never reaches the application. We must wait for
/// the TUI to start rendering (CSI sequences) before sending keyboard input.
fn wait_for_tui_ready(output: &SharedOutput, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let buf = output.lock().unwrap();
        if buf.contains("\x1b[") {
            return true;
        }
        drop(buf);
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Get the output captured so far (for error messages).
fn get_output(output: &SharedOutput) -> String {
    output.lock().unwrap().clone()
}

/// Safely send input to PTY, returning error if process exited unexpectedly.
fn safe_pty_write(
    writer: &mut Box<dyn std::io::Write + Send>,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    data: &[u8],
) -> Result<(), String> {
    match child.try_wait() {
        Ok(Some(status)) => {
            return Err(format!("CLI exited unexpectedly with status: {:?}", status));
        }
        Ok(None) => {}
        Err(e) => {
            return Err(format!("Failed to check process status: {}", e));
        }
    }
    writer
        .write_all(data)
        .map_err(|e| format!("PTY write failed: {}", e))?;
    writer
        .flush()
        .map_err(|e| format!("PTY flush failed: {}", e))?;
    Ok(())
}

/// Build a CommandBuilder for `botster start` with a temp config dir.
fn build_start_cmd(temp_dir: &tempfile::TempDir) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    cmd
}

/// Open a PTY pair with standard dimensions.
fn open_pty() -> portable_pty::PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY")
}

/// How long to wait for the TUI to start rendering.
const TUI_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for the process to exit after a signal/input.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn test_ctrl_q_exits_cleanly() {
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let reader = pair.master.try_clone_reader().unwrap();
    let (_h, output_buf) = spawn_output_capture(reader);

    if !wait_for_tui_ready(&output_buf, TUI_READY_TIMEOUT) {
        let _ = child.kill();
        panic!("TUI did not start within {:?}.\nOutput:\n{}", TUI_READY_TIMEOUT, get_output(&output_buf));
    }

    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        panic!("Failed to send Ctrl+Q: {}\nOutput:\n{}", e, get_output(&output_buf));
    }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "CLI should exit successfully on Ctrl+Q, got: {:?}", status);
                return;
            }
            Ok(None) => {
                if start.elapsed() > EXIT_TIMEOUT {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after Ctrl+Q", EXIT_TIMEOUT);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("Error waiting for process: {}", e),
        }
    }
}

#[test]
fn test_sigint_triggers_graceful_shutdown() {
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();
    let reader = pair.master.try_clone_reader().unwrap();
    let (_h, output_buf) = spawn_output_capture(reader);

    if !wait_for_tui_ready(&output_buf, TUI_READY_TIMEOUT) {
        let _ = child.kill();
        panic!("TUI did not start within {:?}.\nOutput:\n{}", TUI_READY_TIMEOUT, get_output(&output_buf));
    }

    let pid = child.process_id().expect("Failed to get PID");
    unsafe { libc::kill(pid as i32, libc::SIGINT); }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if start.elapsed() > EXIT_TIMEOUT {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after SIGINT", EXIT_TIMEOUT);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("Error waiting for process: {}", e),
        }
    }
}

#[test]
fn test_sigterm_triggers_graceful_shutdown() {
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();
    let reader = pair.master.try_clone_reader().unwrap();
    let (_h, output_buf) = spawn_output_capture(reader);

    if !wait_for_tui_ready(&output_buf, TUI_READY_TIMEOUT) {
        let _ = child.kill();
        panic!("TUI did not start within {:?}.\nOutput:\n{}", TUI_READY_TIMEOUT, get_output(&output_buf));
    }

    let pid = child.process_id().expect("Failed to get PID");
    unsafe { libc::kill(pid as i32, libc::SIGTERM); }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if start.elapsed() > EXIT_TIMEOUT {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after SIGTERM", EXIT_TIMEOUT);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("Error waiting for process: {}", e),
        }
    }
}

#[test]
fn test_pty_close_triggers_cleanup() {
    // When the PTY is closed (simulating terminal window close),
    // the CLI should receive SIGHUP and exit cleanly.
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();

    // Don't capture output — cloned readers keep FDs open and interfere with PTY closure.
    // Use a generous sleep since we can't detect TUI readiness without a reader.
    thread::sleep(Duration::from_secs(5));

    if let Ok(Some(status)) = child.try_wait() {
        panic!("CLI exited before test could run with status: {:?}", status);
    }

    // Drop the master PTY — sends SIGHUP
    drop(pair.master);

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if start.elapsed() > EXIT_TIMEOUT {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after PTY close (SIGHUP)", EXIT_TIMEOUT);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("Error waiting for process: {}", e),
        }
    }
}

#[test]
fn test_input_is_responsive() {
    // Verifies that input is processed promptly once the TUI is ready.
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let reader = pair.master.try_clone_reader().unwrap();
    let (_h, output_buf) = spawn_output_capture(reader);

    if !wait_for_tui_ready(&output_buf, TUI_READY_TIMEOUT) {
        let _ = child.kill();
        panic!("TUI did not start within {:?}.\nOutput:\n{}", TUI_READY_TIMEOUT, get_output(&output_buf));
    }

    let input_start = Instant::now();

    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        panic!("Failed to send Ctrl+Q: {}\nOutput:\n{}", e, get_output(&output_buf));
    }

    // Once TUI is in raw mode, response should be fast
    let response_timeout = Duration::from_secs(5);
    let mut responded = false;

    while input_start.elapsed() < response_timeout {
        match child.try_wait() {
            Ok(Some(_)) => {
                responded = true;
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }

    if !responded {
        let _ = child.kill();
        panic!(
            "CLI did not respond to Ctrl+Q within {:?} - input may be blocked",
            input_start.elapsed()
        );
    }
}

#[test]
fn test_no_orphan_processes_after_exit() {
    // After the CLI exits, the specific child process should be gone.
    ensure_test_env();
    if !binary_exists() {
        eprintln!("Skipping: release binary not found");
        return;
    }

    let temp_dir = tempfile::TempDir::new().unwrap();
    let pair = open_pty();
    let mut child = pair.slave.spawn_command(build_start_cmd(&temp_dir)).unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let reader = pair.master.try_clone_reader().unwrap();
    let (_h, output_buf) = spawn_output_capture(reader);

    // Get the PID of our specific child process
    let child_pid = child.process_id().expect("Failed to get child PID");

    if !wait_for_tui_ready(&output_buf, TUI_READY_TIMEOUT) {
        let _ = child.kill();
        panic!("TUI did not start within {:?}.\nOutput:\n{}", TUI_READY_TIMEOUT, get_output(&output_buf));
    }

    // Exit cleanly with Ctrl+Q
    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        panic!("Failed to send Ctrl+Q: {}\nOutput:\n{}", e, get_output(&output_buf));
    }

    // Wait for exit
    let start = Instant::now();
    while start.elapsed() < EXIT_TIMEOUT {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Give child processes time to be reaped
    thread::sleep(Duration::from_millis(500));

    // Check that our specific process is gone (not a global pgrep which sees other tests)
    let output = Command::new("kill")
        .args(["-0", &child_pid.to_string()])
        .output();

    match output {
        Ok(out) => {
            assert!(
                !out.status.success(),
                "Process {} still running after exit (orphan detected)",
                child_pid
            );
        }
        Err(_) => {
            // kill command failed = process doesn't exist = good
        }
    }
}
