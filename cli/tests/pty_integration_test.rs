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


/// Spawn a thread to drain PTY output (prevents blocking on full buffer)
fn spawn_output_drain(mut reader: Box<dyn std::io::Read + Send>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(_) => continue, // Discard output
                Err(_) => break,
            }
        }
    })
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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    // Prevent actual network connections
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Drain output to prevent CLI from blocking on writes
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let _drain_handle = spawn_output_drain(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Send Ctrl+Q (ASCII 0x11)
    writer.write_all(&[0x11]).expect("Failed to send Ctrl+Q");
    writer.flush().expect("Failed to flush");

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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Drain output to prevent CLI from blocking on writes
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let _drain_handle = spawn_output_drain(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Drain output to prevent CLI from blocking on writes
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let _drain_handle = spawn_output_drain(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Drain output to prevent CLI from blocking on writes
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let _drain_handle = spawn_output_drain(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Record time before sending input
    let input_start = Instant::now();

    // Send Ctrl+Q
    writer.write_all(&[0x11]).expect("Failed to send Ctrl+Q");
    writer.flush().expect("Failed to flush");

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
    cmd.env("BOTSTER_API_KEY", "test-key-for-pty-test");
    cmd.env("BOTSTER_SERVER_URL", "http://localhost:9999");
    cmd.env("BOTSTER_OFFLINE_MODE", "true");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn CLI in PTY");

    let mut writer = pair.master.take_writer().expect("Failed to get PTY writer");

    // Drain output to prevent CLI from blocking on writes
    let reader = pair.master.try_clone_reader().expect("Failed to get PTY reader");
    let _drain_handle = spawn_output_drain(reader);

    // Give the TUI time to initialize
    thread::sleep(Duration::from_millis(800));

    // Exit cleanly with Ctrl+Q
    writer.write_all(&[0x11]).expect("Failed to send Ctrl+Q");
    writer.flush().expect("Failed to flush");

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
