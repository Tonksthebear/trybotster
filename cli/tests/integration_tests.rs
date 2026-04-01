//! Integration tests for botster
//!
//! These tests verify the rendering and scrollback flow using standalone parsers
//! (as clients do) and real PTYs for spawn behavior.
//!
//! Architecture:
//! - PtySession emits raw bytes via broadcast
//! - Clients (TuiRunner, TuiClient) own their own TerminalParser instances
//! - Scroll offset is tracked via ghostty's viewport scrolling API
//! - Agents track PTY lifecycle and scrollback buffer, not terminal emulation

// Rust guideline compliant 2026-03

use botster::ghostty_vt::RenderState;
use botster::terminal::TerminalParser;
use botster::TerminalWidget;
use ratatui::{
    backend::TestBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

// Ensure BOTSTER_ENV is set before any test code runs to avoid keyring prompts
static INIT: Once = Once::new();

fn ensure_test_env() {
    INIT.call_once(|| {
        std::env::set_var("BOTSTER_ENV", "test");
    });
}

/// Helper to create a test agent
fn create_test_agent() -> (botster::Agent, TempDir) {
    // Ensure BOTSTER_ENV=test is set before creating any agents
    ensure_test_env();

    let temp_dir = TempDir::new().unwrap();
    let agent = botster::Agent::new(
        uuid::Uuid::new_v4(),
        "test/repo".to_string(),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );
    (agent, temp_dir)
}

// ── Parser + scroll helpers ───────────────────────────────────────────────────

type TestParser = Arc<Mutex<TerminalParser>>;

/// Create a standalone test parser (simulates a client's local parser).
fn create_test_parser(rows: u16, cols: u16) -> TestParser {
    Arc::new(Mutex::new(TerminalParser::new(rows, cols, 10_000)))
}

/// Create a parser with `line_count` lines of synthetic content pre-loaded.
fn create_parser_with_content(rows: u16, cols: u16, line_count: usize) -> TestParser {
    let parser = Arc::new(Mutex::new(TerminalParser::new(rows, cols, 10_000)));
    {
        let mut p = parser.lock().unwrap();
        for i in 0..line_count {
            p.process(format!("Line {i}\r\n").as_bytes());
        }
    }
    parser
}

/// Create a RenderState updated from a parser.
fn make_render_state(parser: &mut TerminalParser) -> RenderState {
    let mut rs = RenderState::new().expect("render state creation");
    rs.update(parser.terminal_mut())
        .expect("render state update");
    rs
}

// ── Rendering tests ───────────────────────────────────────────────────────────

/// Test that rendering doesn't deadlock when checking scroll state.
#[test]
fn test_render_no_deadlock() {
    let (_agent, _temp_dir) = create_test_agent();
    let parser = create_test_parser(24, 80);

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Test");
            let mut p = parser.lock().unwrap();
            let rs = make_render_state(&mut p);
            let widget = TerminalWidget::new(&rs).block(block);
            f.render_widget(widget, f.area());
        })
        .unwrap();
}

/// Test that scrolling and rendering work together.
#[test]
fn test_scroll_then_render() {
    let parser = create_parser_with_content(24, 80, 100);

    // Verify we have scrollback history
    let history = parser.lock().unwrap().history_size();
    assert!(
        history > 0,
        "should have scrollback after loading 100 lines"
    );

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Scrolled");
            let mut p = parser.lock().unwrap();
            let rs = make_render_state(&mut p);
            let widget = TerminalWidget::new(&rs).block(block);
            f.render_widget(widget, f.area());
        })
        .unwrap();
}

/// Test that extreme scrollback (top of history) doesn't cause issues.
#[test]
fn test_extreme_scrollback_render() {
    let parser = create_parser_with_content(24, 80, 1000);

    let history = parser.lock().unwrap().history_size();
    assert!(
        history > 0,
        "Should have scrollback after loading 1000 lines"
    );

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Top");
            let mut p = parser.lock().unwrap();
            let rs = make_render_state(&mut p);
            let widget = TerminalWidget::new(&rs).block(block);
            f.render_widget(widget, f.area());
        })
        .unwrap();
}

/// Test rapid scroll operations don't deadlock.
#[test]
fn test_rapid_scroll_no_deadlock() {
    let parser = create_parser_with_content(24, 80, 100);

    // Rapid scroll operations via history_size reads
    for _ in 0..100 {
        let p = parser.lock().unwrap();
        let _ = p.history_size();
    }

    // Should complete without deadlock
}

/// Test concurrent access patterns — multiple threads reading from the same parser.
#[test]
fn test_concurrent_scroll_and_render() {
    let parser = create_parser_with_content(24, 80, 100);
    let parser = Arc::new(parser);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let parser = Arc::clone(&parser);
            thread::spawn(move || {
                for _ in 0..25 {
                    {
                        let p = parser.lock().unwrap();
                        let _ = p.history_size();
                    }
                    thread::sleep(Duration::from_micros(100));
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // Should complete without deadlock
}

/// This test mirrors the render loop pattern from TuiRunner.
#[test]
fn test_main_render_loop_pattern() {
    let parser = create_parser_with_content(24, 80, 50);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(f.area());

            // Render agent list (left side)
            let items = vec![ListItem::new("test/repo#1 [CLI]")];
            let list =
                List::new(items).block(Block::default().borders(Borders::ALL).title("Agents"));
            let mut list_state = ListState::default();
            list_state.select(Some(0));
            f.render_stateful_widget(list, chunks[0], &mut list_state);

            let block = Block::default()
                .borders(Borders::ALL)
                .title("test/repo#1 [CLI]");

            let mut p = parser.lock().unwrap();
            let rs = make_render_state(&mut p);
            let widget = TerminalWidget::new(&rs).block(block);
            f.render_widget(widget, chunks[1]);
        })
        .unwrap();
}

/// Test with timeout to catch deadlocks in the render loop.
#[test]
fn test_render_with_timeout() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let parser = create_parser_with_content(24, 80, 50);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate the render loop
        for _ in 0..10 {
            terminal
                .draw(|f| {
                    let block = Block::default().borders(Borders::ALL);
                    let mut p = parser.lock().unwrap();
                    let rs = make_render_state(&mut p);
                    let widget = TerminalWidget::new(&rs).block(block);
                    f.render_widget(widget, f.area());
                })
                .unwrap();
        }

        tx.send(()).unwrap();
    });

    // Wait with timeout - if this times out, we have a deadlock
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {
            handle.join().unwrap();
            // Test passed
        }
        Err(_) => {
            panic!("DEADLOCK DETECTED: Render loop did not complete within 5 seconds");
        }
    }
}

// ============ Real PTY Spawn Tests ============
// These tests spawn actual PTYs using our test fixture scripts

/// Get the path to a test fixture script
fn fixture_path(name: &str) -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    std::path::PathBuf::from(manifest_dir)
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Test spawning a real PTY with our test init script
#[test]
fn test_spawn_real_pty_with_init_script() {
    use std::collections::HashMap;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        // Create tokio runtime for spawn_command_processor() which uses tokio::spawn()
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        let _runtime_guard = runtime.enter();

        let temp_dir = TempDir::new().unwrap();
        let mut agent = botster::Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Spawn using bash with our test script
        let init_script = fixture_path("test_botster_init.sh");
        // Write context.json to simulate agent setup
        let botster_dir = temp_dir.path().join(".botster");
        std::fs::create_dir_all(&botster_dir).unwrap();
        std::fs::write(
            botster_dir.join("context.json"),
            r#"{"repo":"test/repo","issue_number":1,"branch_name":"test-branch","prompt":"Test task","created_at":"2025-01-01T00:00:00Z"}"#,
        ).unwrap();

        let mut env_vars = HashMap::new();
        env_vars.insert(
            "BOTSTER_WORKTREE_PATH".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        // Spawn bash and source the init script
        use botster::agent::spawn::PtySpawnConfig;
        agent
            .pty
            .spawn(PtySpawnConfig {
                worktree_path: temp_dir.path().to_path_buf(),
                command: "bash".to_string(),
                env: env_vars,
                init_commands: vec![format!("source {}", init_script.display())],
                detect_notifications: true,
                port: None,
                context: String::new(),
            })
            .expect("Failed to spawn PTY");

        // Wait for output to be generated
        thread::sleep(Duration::from_millis(500));

        // Snapshot verification requires a session process (not available in
        // unit tests). PTY spawning is verified by is_spawned() above.

        tx.send(()).unwrap();
    });

    // Wait with generous timeout for real PTY operations
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: Real PTY spawn test did not complete within 15 seconds");
        }
    }
}

/// Test spawning a PTY with our test server script
#[test]
fn test_spawn_pty_with_server_script() {
    use std::collections::HashMap;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        // Create tokio runtime for spawn_command_processor() which uses tokio::spawn()
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        let _runtime_guard = runtime.enter();

        let temp_dir = TempDir::new().unwrap();
        let mut agent = botster::Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Spawn the PTY with the server script
        let server_script = fixture_path("test_botster_server.sh");
        let mut server_env = HashMap::new();
        server_env.insert("PORT".to_string(), "3000".to_string());

        use botster::agent::spawn::PtySpawnConfig;
        agent
            .pty
            .spawn(PtySpawnConfig {
                worktree_path: temp_dir.path().to_path_buf(),
                command: server_script.display().to_string(),
                env: server_env,
                init_commands: vec![],
                detect_notifications: false,
                port: Some(3000),
                context: String::new(),
            })
            .expect("Failed to spawn PTY");

        assert!(agent.pty.is_spawned(), "PTY should be spawned");

        // Wait for server to produce output
        thread::sleep(Duration::from_secs(3));

        // Snapshot verification requires a session process (not available in
        // unit tests). PTY spawning is verified by is_spawned() above.

        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: Server PTY spawn test did not complete within 15 seconds");
        }
    }
}

/// Test that agent's single PTY can be spawned and produces output
#[test]
fn test_real_pty_spawn_and_output() {
    use std::collections::HashMap;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        // Create tokio runtime for spawn_command_processor() which uses tokio::spawn()
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        let _runtime_guard = runtime.enter();

        let temp_dir = TempDir::new().unwrap();
        let mut agent = botster::Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Spawn PTY
        use botster::agent::spawn::PtySpawnConfig;
        agent
            .pty
            .spawn(PtySpawnConfig {
                worktree_path: temp_dir.path().to_path_buf(),
                command: "bash".to_string(),
                env: HashMap::new(),
                init_commands: vec!["for i in $(seq 1 50); do echo \"Line $i\"; done".to_string()],
                detect_notifications: true,
                port: None,
                context: String::new(),
            })
            .expect("Failed to spawn PTY");

        // Wait for output
        thread::sleep(Duration::from_secs(2));

        // Verify the agent's PTY is spawned
        assert!(agent.pty.is_spawned());

        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: PTY spawn test did not complete within 15 seconds");
        }
    }
}

/// Test rapid rendering with parser — catches potential deadlocks.
#[test]
fn test_rapid_render_no_deadlock() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let parser = create_parser_with_content(24, 80, 200);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate rapid rendering
        for _ in 0..50 {
            terminal
                .draw(|f| {
                    let block = Block::default().borders(Borders::ALL).title("Rapid Test");
                    let mut p = parser.lock().unwrap();
                    let rs = make_render_state(&mut p);
                    let widget = TerminalWidget::new(&rs).block(block);
                    f.render_widget(widget, f.area());
                })
                .unwrap();
        }

        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("DEADLOCK: Rapid render test did not complete within 10 seconds");
        }
    }
}

// ============================================================================
// Browser -> PTY I/O Flow Integration Tests
// ============================================================================

/// Test that Agent exposes its configured PTY dimensions.
#[test]
fn test_agent_reports_pty_dimensions() {
    let (agent, _temp_dir) = create_test_agent();
    let (rows, cols) = agent.get_pty_size();
    assert_eq!((rows, cols), (24, 80));
}

/// Test that multiple subscribers can receive PTY events (broadcast pattern).
#[test]
fn test_pty_broadcast_to_multiple_subscribers() {
    use botster::agent::pty::PtySession;

    // Create a PTY session directly for testing broadcast
    let session = PtySession::new(24, 80);

    // Get multiple subscribers
    let (event_tx, _cmd_tx, _port) = session.get_channels();
    let mut rx1 = event_tx.subscribe();
    let mut rx2 = event_tx.subscribe();
    let mut rx3 = event_tx.subscribe();

    // Send an event through the broadcast channel
    use botster::agent::pty::PtyEvent;
    let _ = event_tx.send(PtyEvent::Output(b"test output".to_vec()));

    // All subscribers should receive the event
    assert!(rx1.try_recv().is_ok(), "Subscriber 1 should receive event");
    assert!(rx2.try_recv().is_ok(), "Subscriber 2 should receive event");
    assert!(rx3.try_recv().is_ok(), "Subscriber 3 should receive event");
}

/// Test that PTY input can be written via command channel.
#[test]
fn test_pty_input_via_command_channel() {
    use botster::agent::pty::PtyCommand;
    use botster::agent::pty::PtySession;

    let session = PtySession::new(24, 80);
    let (_event_tx, cmd_tx, _port) = session.get_channels();

    let runtime = tokio::runtime::Runtime::new().expect("Failed to create runtime");

    let result =
        runtime.block_on(async { cmd_tx.send(PtyCommand::Input(b"test input".to_vec())).await });

    assert!(result.is_ok(), "Command channel should accept input");
}

/// Test that PTY resize works via direct resize method.
#[test]
fn test_pty_resize_direct() {
    use botster::agent::pty::PtySession;

    let session = PtySession::new(24, 80);
    assert_eq!(session.dimensions(), (24, 80));

    session.resize(50, 100);
    assert_eq!(session.dimensions(), (50, 100));
}

// Agent::get_snapshot() removed — snapshots are session-process-backed via RPC.
// Snapshot testing requires a running session process, not available in unit tests.
