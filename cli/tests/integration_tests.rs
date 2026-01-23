//! Integration tests for botster-hub
//!
//! These tests verify the rendering and scrollback flow using standalone parsers
//! (as clients do) and real PTYs for spawn behavior.
//!
//! Architecture (Phase 5):
//! - PtySession emits raw bytes via broadcast
//! - Clients (TuiRunner, TuiClient) own their own vt100 parsers
//! - Scroll operations work on parser references
//! - Agents track PTY lifecycle and scrollback buffer, not terminal emulation

use botster_hub::tui::scroll;
use botster_hub::{Agent, PtyView, TerminalWidget};
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
use vt100::Parser;

// Ensure BOTSTER_ENV is set before any test code runs to avoid keyring prompts
static INIT: Once = Once::new();

fn ensure_test_env() {
    INIT.call_once(|| {
        std::env::set_var("BOTSTER_ENV", "test");
    });
}

/// Helper to create a test agent
fn create_test_agent() -> (Agent, TempDir) {
    // Ensure BOTSTER_ENV=test is set before creating any agents
    ensure_test_env();

    let temp_dir = TempDir::new().unwrap();
    let agent = Agent::new(
        uuid::Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        temp_dir.path().to_path_buf(),
    );
    (agent, temp_dir)
}

/// Create a standalone test parser (simulates client's parser).
fn create_test_parser(rows: u16, cols: u16) -> Arc<Mutex<Parser>> {
    Arc::new(Mutex::new(Parser::new(rows, cols, 10000)))
}

/// Create a parser with content pre-loaded.
fn create_parser_with_content(rows: u16, cols: u16, line_count: usize) -> Arc<Mutex<Parser>> {
    let parser = Arc::new(Mutex::new(Parser::new(rows, cols, 10000)));
    {
        let mut p = parser.lock().unwrap();
        for i in 0..line_count {
            p.process(format!("Line {}\r\n", i).as_bytes());
        }
    }
    parser
}

/// Test that rendering doesn't deadlock when checking scroll state
#[test]
fn test_render_no_deadlock() {
    let (_agent, _temp_dir) = create_test_agent();
    let parser = create_test_parser(24, 80);

    // Simulate the render loop pattern
    let parser_lock = parser.lock().unwrap();
    let screen = parser_lock.screen();

    // This is the pattern that caused deadlocks - checking scrollback
    // from the already-held lock instead of calling is_scrolled() separately
    let is_scrolled = screen.scrollback() > 0;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Test");
            let widget = if is_scrolled {
                TerminalWidget::new(screen).block(block).hide_cursor()
            } else {
                TerminalWidget::new(screen).block(block)
            };
            f.render_widget(widget, f.area());
        })
        .unwrap();
}

/// Test that scrolling and rendering work together using parser-based API
#[test]
fn test_scroll_then_render() {
    let parser = create_parser_with_content(24, 80, 100);

    // Scroll up using parser-based scroll module
    scroll::up_parser(&parser, 20);
    assert!(scroll::is_scrolled_parser(&parser));

    // Now render - this should not deadlock
    let parser_lock = parser.lock().unwrap();
    let screen = parser_lock.screen();
    let is_scrolled = screen.scrollback() > 0;
    assert!(is_scrolled, "Screen should show scrollback state");

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Scrolled");
            let widget = TerminalWidget::new(screen).block(block).hide_cursor();
            f.render_widget(widget, f.area());
        })
        .unwrap();
}

/// Test that switching PTY views works (scroll is now per-parser)
#[test]
fn test_pty_view_switch() {
    use botster_hub::agent::PtySession;

    let (mut agent, _temp_dir) = create_test_agent();
    agent.server_pty = Some(PtySession::new(24, 80));

    // Phase 5: PTY view is now per-client state, not per-agent.
    // Test that agent.has_server_pty() returns true when server PTY exists.
    assert!(agent.has_server_pty());

    // Clients track their own active_pty_view and use toggle_pty_view() method.
    // See client tests (tui.rs, browser.rs) for client-side toggle testing.
}

/// Test extreme scrollback doesn't cause issues
#[test]
fn test_extreme_scrollback_render() {
    let parser = create_parser_with_content(24, 80, 1000);

    // Scroll to top
    scroll::to_top_parser(&parser);
    assert!(scroll::is_scrolled_parser(&parser));

    // Render
    let parser_lock = parser.lock().unwrap();
    let screen = parser_lock.screen();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let block = Block::default().borders(Borders::ALL).title("Top");
            let widget = TerminalWidget::new(screen).block(block).hide_cursor();
            f.render_widget(widget, f.area());
        })
        .unwrap();

    // Verify we're showing early content
    let buffer = terminal.backend().buffer();
    let first_char = buffer[(1, 1)].symbol();
    assert!(
        first_char == "L" || first_char == " ",
        "Expected line content or space, got {:?}",
        first_char
    );
}

/// Test rapid scroll operations don't deadlock
#[test]
fn test_rapid_scroll_no_deadlock() {
    let parser = create_parser_with_content(24, 80, 100);

    // Rapid scroll operations
    for _ in 0..100 {
        scroll::up_parser(&parser, 3);
        scroll::down_parser(&parser, 1);

        // Simulate render check
        let parser_lock = parser.lock().unwrap();
        let _is_scrolled = parser_lock.screen().scrollback() > 0;
    }

    // Should complete without deadlock
}

/// Test concurrent access patterns
#[test]
fn test_concurrent_scroll_and_render() {
    // For concurrent tests, we use raw parsers since Agent isn't Send/Sync.
    // This tests the underlying parser thread safety.
    let parser = create_parser_with_content(24, 80, 100);

    // Clone parser for threads
    let parser = Arc::new(parser);

    // Spawn threads that manipulate parser directly
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let parser = Arc::clone(&parser);
            thread::spawn(move || {
                for _ in 0..25 {
                    // Directly manipulate parser
                    {
                        let mut p = parser.lock().unwrap();
                        let current = p.screen().scrollback();
                        if i % 2 == 0 {
                            p.screen_mut().set_scrollback(current.saturating_add(1));
                        } else {
                            p.screen_mut().set_scrollback(current.saturating_sub(1));
                        }
                    }
                    // Check scroll state
                    {
                        let p = parser.lock().unwrap();
                        let _ = p.screen().scrollback() > 0;
                        let _ = p.screen().scrollback();
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

/// This test mirrors the render loop pattern
#[test]
fn test_main_render_loop_pattern() {
    let parser = create_parser_with_content(24, 80, 50);

    // Scroll up so we're in scrollback mode
    scroll::up_parser(&parser, 10);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    // This mirrors the render code pattern
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

            // Build terminal title with scroll indicator
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();
            let is_scrolled = screen.scrollback() > 0;
            let scroll_offset = screen.scrollback();

            let scroll_indicator = if is_scrolled {
                format!(" [SCROLLBACK +{}]", scroll_offset)
            } else {
                String::new()
            };

            let terminal_title = format!("test/repo#1 [CLI]{}", scroll_indicator);
            let block = Block::default().borders(Borders::ALL).title(terminal_title);

            let widget = if is_scrolled {
                TerminalWidget::new(screen).block(block).hide_cursor()
            } else {
                TerminalWidget::new(screen).block(block)
            };
            f.render_widget(widget, chunks[1]);
        })
        .unwrap();

    // If we get here, no deadlock occurred
}

/// Test with timeout to catch deadlocks
#[test]
fn test_render_with_timeout() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let parser = create_parser_with_content(24, 80, 50);

        scroll::up_parser(&parser, 10);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate the render loop
        for _ in 0..10 {
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();
            let is_scrolled = screen.scrollback() > 0;

            terminal
                .draw(|f| {
                    let block = Block::default().borders(Borders::ALL);
                    let widget = if is_scrolled {
                        TerminalWidget::new(screen).block(block).hide_cursor()
                    } else {
                        TerminalWidget::new(screen).block(block)
                    };
                    f.render_widget(widget, f.area());
                })
                .unwrap();

            drop(parser_lock);

            // Scroll between renders
            scroll::down_parser(&parser, 1);
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
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Spawn using bash with our test script
        let init_script = fixture_path("test_botster_init.sh");
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "BOTSTER_WORKTREE_PATH".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );
        env_vars.insert(
            "BOTSTER_TASK_DESCRIPTION".to_string(),
            "Test task".to_string(),
        );
        env_vars.insert("BOTSTER_BRANCH_NAME".to_string(), "test-branch".to_string());

        // Spawn bash and source the init script
        agent
            .spawn(
                "bash",
                "",
                vec![format!("source {}", init_script.display())],
                &env_vars,
            )
            .expect("Failed to spawn PTY");

        // Wait for output to be generated
        thread::sleep(Duration::from_millis(500));

        // Verify we received some output via scrollback buffer
        let buffer = agent.get_scrollback_snapshot(PtyView::Cli);
        assert!(!buffer.is_empty(), "PTY should have produced output");

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

/// Test spawning the server PTY with our test server script
#[test]
fn test_spawn_server_pty() {
    use std::collections::HashMap;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // First set up the CLI PTY (as would happen in normal operation)
        let empty_env = HashMap::new();
        agent
            .spawn(
                "bash",
                "",
                vec!["echo 'CLI PTY started'".to_string()],
                &empty_env,
            )
            .expect("Failed to spawn CLI PTY");

        // Now spawn the server PTY
        let server_script = fixture_path("test_botster_server.sh");
        let mut server_env = HashMap::new();
        server_env.insert("BOTSTER_TUNNEL_PORT".to_string(), "3000".to_string());

        agent
            .spawn_server_pty(&server_script.display().to_string(), &server_env)
            .expect("Failed to spawn server PTY");

        assert!(agent.has_server_pty(), "Server PTY should be available");

        // Wait for server to produce output
        thread::sleep(Duration::from_secs(3));

        // Check server PTY content
        let server_buffer = agent.server_pty.as_ref().unwrap().get_scrollback_snapshot();
        assert!(!server_buffer.is_empty(), "Server PTY should have output");

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

/// Test switching between CLI and Server PTY views with real PTYs
#[test]
fn test_real_pty_view_switching() {
    use std::collections::HashMap;
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(1),
            "test-branch".to_string(),
            temp_dir.path().to_path_buf(),
        );

        // Spawn CLI PTY
        let empty_env = HashMap::new();
        agent
            .spawn(
                "bash",
                "",
                vec!["for i in $(seq 1 50); do echo \"CLI Line $i\"; done".to_string()],
                &empty_env,
            )
            .expect("Failed to spawn CLI PTY");

        // Spawn server PTY
        let server_script = fixture_path("test_botster_server.sh");
        agent
            .spawn_server_pty(&server_script.display().to_string(), &empty_env)
            .expect("Failed to spawn server PTY");

        // Wait for output
        thread::sleep(Duration::from_secs(2));

        // Phase 5: PTY view is now per-client state, not per-agent.
        // Verify that the agent has both PTYs available.
        assert!(agent.cli_pty.is_spawned());
        assert!(agent.has_server_pty());

        // Clients track their own active_pty_view and use write_input_to_view().
        // See client tests (tui.rs, browser.rs) for client-side toggle testing.

        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: PTY view switching test did not complete within 15 seconds");
        }
    }
}

/// Test rapid scrolling with parser - catches potential deadlocks
#[test]
fn test_rapid_scroll_parser_no_deadlock() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let parser = create_parser_with_content(24, 80, 200);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate rapid scrolling like a user would do
        for i in 0..50 {
            // Scroll using parser-based API
            if i % 3 == 0 {
                scroll::up_parser(&parser, 5);
            } else if i % 3 == 1 {
                scroll::down_parser(&parser, 2);
            } else {
                scroll::to_top_parser(&parser);
                scroll::down_parser(&parser, 10);
            }

            // Lock and render
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();
            let is_scrolled = screen.scrollback() > 0;
            let scroll_offset = screen.scrollback();

            let scroll_indicator = if is_scrolled {
                format!(" [SCROLLBACK +{}]", scroll_offset)
            } else {
                String::new()
            };

            terminal
                .draw(|f| {
                    let title = format!("Rapid Scroll Test{}", scroll_indicator);
                    let block = Block::default().borders(Borders::ALL).title(title);
                    let widget = if is_scrolled {
                        TerminalWidget::new(screen).block(block).hide_cursor()
                    } else {
                        TerminalWidget::new(screen).block(block)
                    };
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
            panic!("DEADLOCK: Rapid scroll test did not complete within 10 seconds");
        }
    }
}
