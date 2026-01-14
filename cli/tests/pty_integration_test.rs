// PTY-based integration tests for TUI behavior
//
// These tests spawn the CLI inside a pseudo-TTY, allowing us to test
// interactive behavior that requires a real terminal.
//
// Run with: cargo test --test pty_integration_test -- --test-threads=1
//
// IMPORTANT: Run `cargo build --release` before running these tests!

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Path to the release binary
fn get_binary_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps
    path.pop(); // Remove debug
    path.push("release");
    path.push("botster-hub");
    path
}

/// Check if the release binary exists
fn binary_exists() -> bool {
    get_binary_path().exists()
}

/// Check if any botster-hub processes are running (excluding our test)
fn count_botster_processes() -> usize {
    let output = Command::new("pgrep")
        .args(["-f", "botster-hub"])
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().count()
        }
        Err(_) => 0,
    }
}


/// Spawn a thread to capture PTY output (prevents blocking on full buffer)
/// Returns a handle and a receiver for the captured output
fn spawn_output_capture(
    mut reader: Box<dyn std::io::Read + Send>,
) -> (thread::JoinHandle<()>, std::sync::mpsc::Receiver<String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut output = String::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(output);
    });
    (handle, rx)
}

/// Safely send input to PTY, returning error if process exited unexpectedly.
fn safe_pty_write(
    writer: &mut Box<dyn std::io::Write + Send>,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    data: &[u8],
) -> Result<(), String> {
    // Check if process is still running before sending input
    match child.try_wait() {
        Ok(Some(status)) => {
            return Err(format!("CLI exited unexpectedly with status: {:?}", status));
        }
        Ok(None) => {
            // Process still running, continue
        }
        Err(e) => {
            return Err(format!("Failed to check process status: {}", e));
        }
    }

    // Try to write
    writer.write_all(data).map_err(|e| format!("PTY write failed: {}", e))?;
    writer.flush().map_err(|e| format!("PTY flush failed: {}", e))?;
    Ok(())
}

#[test]
fn test_ctrl_q_exits_cleanly() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Capture output to see errors if CLI fails
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let (_capture_handle, output_rx) = spawn_output_capture(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Send Ctrl+Q (ASCII 0x11)
    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        // Get captured output to show in error message
        let output = output_rx.recv_timeout(Duration::from_millis(100)).unwrap_or_default();
        panic!("Failed to send input to CLI: {}\nCLI output:\n{}", e, output);
    }

    // Wait for process to exit (should be quick)
    let start = Instant::now();
    let timeout = Duration::from_secs(3);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited
                assert!(
                    status.success(),
                    "CLI should exit successfully on Ctrl+Q, got: {:?}",
                    status
                );
                return;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    // Force kill and fail
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after Ctrl+Q", timeout);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                panic!("Error waiting for process: {}", e);
            }
        }
    }
}

#[test]
fn test_sigint_triggers_graceful_shutdown() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Capture output to see errors if CLI fails
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let (_capture_handle, output_rx) = spawn_output_capture(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Check if process is still running before sending signal
    if let Ok(Some(status)) = child.try_wait() {
        let output = output_rx.recv_timeout(Duration::from_millis(100)).unwrap_or_default();
        panic!("CLI exited before test could run with status: {:?}\nCLI output:\n{}", status, output);
    }

    // Get the process ID and send SIGINT
    let pid = child.process_id().expect("Failed to get PID");

    unsafe {
        libc::kill(pid as i32, libc::SIGINT);
    }

    // Wait for process to exit
    let start = Instant::now();
    let timeout = Duration::from_secs(5);

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Process exited - success (exit code may vary with signal)
                return;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after SIGINT", timeout);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                panic!("Error waiting for process: {}", e);
            }
        }
    }
}

#[test]
fn test_sigterm_triggers_graceful_shutdown() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Capture output to see errors if CLI fails
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let (_capture_handle, output_rx) = spawn_output_capture(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Check if process is still running before sending signal
    if let Ok(Some(status)) = child.try_wait() {
        let output = output_rx.recv_timeout(Duration::from_millis(100)).unwrap_or_default();
        panic!("CLI exited before test could run with status: {:?}\nCLI output:\n{}", status, output);
    }

    // Send SIGTERM
    let pid = child.process_id().expect("Failed to get PID");

    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    // Wait for process to exit
    let start = Instant::now();
    let timeout = Duration::from_secs(5);

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    panic!("CLI did not exit within {:?} after SIGTERM", timeout);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                panic!("Error waiting for process: {}", e);
            }
        }
    }
}

#[test]
fn test_pty_close_triggers_cleanup() {
    // When the PTY is closed (simulating terminal window close),
    // the CLI should receive SIGHUP and exit cleanly.
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Note: We intentionally don't capture output here because the test requires
    // dropping the PTY master cleanly. Any cloned readers would keep FDs open
    // and potentially interfere with PTY closure semantics.

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Check if process is still running before closing PTY
    if let Ok(Some(status)) = child.try_wait() {
        panic!("CLI exited before test could run with status: {:?}", status);
    }

    // Drop the master PTY - this closes the terminal, sending SIGHUP
    drop(pair.master);

    // Wait for process to exit
    let start = Instant::now();
    let timeout = Duration::from_secs(5);

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Process exited after PTY close
                return;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    panic!(
                        "CLI did not exit within {:?} after PTY close (SIGHUP)",
                        timeout
                    );
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                panic!("Error waiting for process: {}", e);
            }
        }
    }
}

#[test]
fn test_input_is_responsive() {
    // This test verifies that input is processed promptly and not blocked.
    // We send multiple Ctrl+Q signals and verify the CLI exits quickly.
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Capture output to see errors if CLI fails
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let (_capture_handle, output_rx) = spawn_output_capture(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Record time before sending input
    let input_start = Instant::now();

    // Send Ctrl+Q
    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        let output = output_rx.recv_timeout(Duration::from_millis(100)).unwrap_or_default();
        panic!("Failed to send input to CLI: {}\nCLI output:\n{}", e, output);
    }

    // The CLI should respond within 500ms if input handling is working
    let response_timeout = Duration::from_millis(500);
    let mut responded = false;

    while input_start.elapsed() < response_timeout {
        match child.try_wait() {
            Ok(Some(_)) => {
                responded = true;
                break;
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }

    if !responded {
        // If not responded yet, wait a bit more and then fail
        thread::sleep(Duration::from_millis(500));
        match child.try_wait() {
            Ok(Some(_)) => {
                // It did respond, but slowly
                let elapsed = input_start.elapsed();
                if elapsed > Duration::from_secs(1) {
                    panic!(
                        "Input response was too slow: {:?} (should be < 500ms)",
                        elapsed
                    );
                }
            }
            Ok(None) => {
                let _ = child.kill();
                panic!(
                    "CLI did not respond to Ctrl+Q within {:?} - input may be blocked",
                    input_start.elapsed()
                );
            }
            Err(e) => {
                panic!("Error: {}", e);
            }
        }
    }
}

#[test]
fn test_no_orphan_processes_after_exit() {
    // After the CLI exits, there should be no orphaned child processes
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    // Count existing botster processes before test
    let before_count = count_botster_processes();

    let pty_system = native_pty_system();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new(get_binary_path());
    cmd.arg("start");
    cmd.env("BOTSTER_CONFIG_DIR", temp_dir.path());
    // Use test mode to skip authentication and network connections
    cmd.env("BOTSTER_ENV", "test");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Capture output to see errors if CLI fails
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let (_capture_handle, output_rx) = spawn_output_capture(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Exit cleanly with Ctrl+Q
    if let Err(e) = safe_pty_write(&mut writer, &mut child, &[0x11]) {
        let output = output_rx.recv_timeout(Duration::from_millis(100)).unwrap_or_default();
        panic!("Failed to send input to CLI: {}\nCLI output:\n{}", e, output);
    }

    // Wait for exit
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Give any child processes time to be reaped
    thread::sleep(Duration::from_millis(500));

    // Count processes after
    let after_count = count_botster_processes();

    assert!(
        after_count <= before_count,
        "Orphan processes detected! Before: {}, After: {}",
        before_count,
        after_count
    );
}
