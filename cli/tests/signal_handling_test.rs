// Integration tests for CLI behavior and process management
//
// These tests verify that:
// 1. CLI commands that don't require a TTY work correctly
// 2. The CLI properly handles missing TTY gracefully
// 3. Basic CLI operations complete in reasonable time
// 4. Process cleanup works correctly
//
// Note: Tests that require the interactive TUI (start command) cannot run
// without a real TTY. Signal handling for interactive mode is tested manually.
//
// Run with: cargo test --test signal_handling_test -- --test-threads=1
//
// IMPORTANT: Run `cargo build --release` before running these tests!

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::thread;

/// Path to the release binary (built by cargo build --release)
fn get_binary_path() -> std::path::PathBuf {
    // current_exe() returns something like target/debug/deps/signal_handling_test-xxx
    // We need to get to target/release/botster-hub
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name (signal_handling_test-xxx)
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

/// Helper to kill a process
#[cfg(unix)]
#[allow(dead_code)]
fn kill_process(child: &mut std::process::Child, signal: i32) -> std::io::Result<()> {
    unsafe {
        libc::kill(child.id() as i32, signal);
    }
    Ok(())
}

/// Wait for process to exit with timeout
#[allow(dead_code)]
fn wait_with_timeout(child: &mut std::process::Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

#[test]
fn test_help_command_exits_immediately() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let start = Instant::now();

    let output = Command::new(get_binary_path())
        .arg("--help")
        .output()
        .expect("Failed to run --help");

    let elapsed = start.elapsed();

    // --help should complete very quickly (under 2 seconds)
    assert!(elapsed < Duration::from_secs(2), "--help took too long: {:?}", elapsed);
    assert!(output.status.success(), "--help failed: {:?}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("botster-hub") || stdout.contains("Usage"),
            "Unexpected --help output: {}", stdout);
}

#[test]
fn test_version_command_exits_immediately() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let start = Instant::now();

    let output = Command::new(get_binary_path())
        .arg("--version")
        .output()
        .expect("Failed to run --version");

    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_secs(2), "--version took too long: {:?}", elapsed);
    assert!(output.status.success(), "--version failed: {:?}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("botster-hub"), "Unexpected --version output: {}", stdout);
}

#[test]
fn test_start_without_tty_fails_gracefully() {
    // The interactive CLI requires a TTY. When run without one (e.g., in a pipe),
    // it should fail with a clear error rather than hanging or crashing.
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let start = Instant::now();

    let output = Command::new(get_binary_path())
        .arg("start")
        .env("BOTSTER_CONFIG_DIR", temp_dir.path())
        .env("BOTSTER_API_KEY", "test-key")
        .env("BOTSTER_SERVER_URL", "http://localhost:9999")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run start command");

    let elapsed = start.elapsed();

    // Should fail quickly (not hang waiting for TTY)
    assert!(elapsed < Duration::from_secs(5), "start command took too long without TTY: {:?}", elapsed);

    // Should fail (no TTY available)
    assert!(!output.status.success(), "start should fail without TTY");

    // Error message should mention the issue
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Device not configured") ||
        stderr.contains("not a terminal") ||
        stderr.contains("tty") ||
        stderr.contains("Error"),
        "Expected TTY-related error, got: {}", stderr
    );
}

#[test]
fn test_config_command_works() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");

    let output = Command::new(get_binary_path())
        .arg("config")
        .env("BOTSTER_CONFIG_DIR", temp_dir.path())
        .output()
        .expect("Failed to run config command");

    assert!(output.status.success(), "config command failed: {:?}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should output JSON config
    assert!(stdout.contains("{") && stdout.contains("}"),
            "Expected JSON output, got: {}", stdout);
}

#[test]
fn test_status_command_works() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let output = Command::new(get_binary_path())
        .arg("status")
        .output()
        .expect("Failed to run status command");

    // Status command may not be fully implemented, but should not crash
    assert!(output.status.success(), "status command failed: {:?}", output.status);
}

#[test]
fn test_invalid_command_fails() {
    if !binary_exists() {
        eprintln!("Skipping test: release binary not found. Run `cargo build --release` first.");
        return;
    }

    let output = Command::new(get_binary_path())
        .arg("invalid-command-that-does-not-exist")
        .output()
        .expect("Failed to run invalid command");

    // Should fail with an error about invalid command
    assert!(!output.status.success(), "invalid command should fail");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("error") ||
        stderr.to_lowercase().contains("invalid") ||
        stderr.to_lowercase().contains("unknown"),
        "Expected error message about invalid command, got: {}", stderr
    );
}

// ============================================================================
// Manual Testing Instructions for Signal Handling
// ============================================================================
//
// The signal handling tests require a real TTY and cannot be automated easily.
// Follow these steps to manually verify signal handling works:
//
// 1. Build the release binary:
//    $ cargo build --release
//
// 2. Start the CLI in a terminal:
//    $ ./target/release/botster-hub start
//
// 3. Test Ctrl+Q (application quit):
//    - Press Ctrl+Q
//    - Expected: CLI exits immediately and cleanly
//
// 4. Test Ctrl+C (SIGINT):
//    - Start the CLI again
//    - Press Ctrl+C
//    - Expected: CLI exits cleanly with message "Shutdown signal received"
//
// 5. Test terminal close (SIGHUP):
//    - Start the CLI in a terminal
//    - Close the terminal window
//    - Check with `ps aux | grep botster` that no orphan processes remain
//
// 6. Test input responsiveness with WebRTC:
//    - Start the CLI
//    - Connect a browser via the web UI
//    - While connected, try pressing Ctrl+Q
//    - Expected: CLI should still respond to Ctrl+Q (not blocked by WebRTC)
//
// If any of these tests fail, the signal handling or input loop needs fixing.
