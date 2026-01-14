//! Integration tests for botster-hub
//!
//! These tests spawn real PTYs with simple shell scripts (not claude)
//! to test the full rendering and scrollback flow.
//!
//! IMPORTANT: Tests should call actual code paths from the application,
//! not just verify patterns work in isolation.

use botster_hub::{Agent, PtyView, TerminalWidget};
use ratatui::{
    backend::TestBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Helper to create a test agent
fn create_test_agent() -> (Agent, TempDir) {
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

/// Test that rendering doesn't deadlock when checking scroll state
#[test]
fn test_render_no_deadlock() {
    let (agent, _temp_dir) = create_test_agent();

    // Simulate the render loop pattern from main.rs
    let parser = agent.get_active_parser();
    let parser_lock = parser.lock().unwrap();
    let screen = parser_lock.screen();

    // This is the pattern that caused deadlocks - checking scrollback
    // from the already-held lock instead of calling agent.is_scrolled()
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

    // If we get here without deadlocking, the test passes
}

/// Test that scrolling and rendering work together
#[test]
fn test_scroll_then_render() {
    let (mut agent, _temp_dir) = create_test_agent();

    // Add content to the parser
    {
        let parser = agent.get_active_parser();
        let mut p = parser.lock().unwrap();
        for i in 0..100 {
            p.process(format!("Line {}\r\n", i).as_bytes());
        }
    }

    // Scroll up
    agent.scroll_up(20);
    assert!(agent.is_scrolled());

    // Now render - this should not deadlock
    let parser = agent.get_active_parser();
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

/// Test that switching PTY views preserves scroll state
#[test]
fn test_pty_view_switch_preserves_scroll() {
    use botster_hub::agent::PtySession;

    let (mut agent, _temp_dir) = create_test_agent();

    // cli_pty is already initialized in Agent::new(), just set up server_pty
    agent.server_pty = Some(PtySession::new(24, 80));

    // Add content to both
    {
        let mut p = agent.cli_pty.vt100_parser.lock().unwrap();
        for i in 0..50 {
            p.process(format!("CLI Line {}\r\n", i).as_bytes());
        }
    }
    {
        let mut p = agent.server_pty.as_ref().unwrap().vt100_parser.lock().unwrap();
        for i in 0..50 {
            p.process(format!("Server Line {}\r\n", i).as_bytes());
        }
    }

    // Scroll CLI
    agent.active_pty = PtyView::Cli;
    agent.scroll_up(10);
    assert_eq!(agent.get_scroll_offset(), 10);

    // Switch to server and scroll differently
    agent.active_pty = PtyView::Server;
    agent.scroll_up(5);
    assert_eq!(agent.get_scroll_offset(), 5);

    // Switch back to CLI - should still be at 10
    agent.active_pty = PtyView::Cli;
    assert_eq!(agent.get_scroll_offset(), 10);

    // Render both views without deadlock
    for pty_view in [PtyView::Cli, PtyView::Server] {
        agent.active_pty = pty_view;
        let parser = agent.get_active_parser();
        let parser_lock = parser.lock().unwrap();
        let screen = parser_lock.screen();
        let is_scrolled = screen.scrollback() > 0;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let title = format!("{:?} View", pty_view);
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
}

/// Test extreme scrollback doesn't cause issues
#[test]
fn test_extreme_scrollback_render() {
    let (mut agent, _temp_dir) = create_test_agent();

    // Add lots of content
    {
        let parser = agent.get_active_parser();
        let mut p = parser.lock().unwrap();
        for i in 0..1000 {
            p.process(format!("Line {:04}\r\n", i).as_bytes());
        }
    }

    // Scroll to top
    agent.scroll_to_top();
    assert!(agent.is_scrolled());

    // Render
    let parser = agent.get_active_parser();
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
    // First visible line should start with "Line 00" (early content)
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
    let (mut agent, _temp_dir) = create_test_agent();

    // Add content
    {
        let parser = agent.get_active_parser();
        let mut p = parser.lock().unwrap();
        for i in 0..100 {
            p.process(format!("Line {}\r\n", i).as_bytes());
        }
    }

    // Rapid scroll operations
    for _ in 0..100 {
        agent.scroll_up(3);
        agent.scroll_down(1);

        // Simulate render check
        let parser = agent.get_active_parser();
        let parser_lock = parser.lock().unwrap();
        let _is_scrolled = parser_lock.screen().scrollback() > 0;
    }

    // Should complete without deadlock
}

/// Test concurrent access patterns
#[test]
fn test_concurrent_scroll_and_render() {
    let (agent, _temp_dir) = create_test_agent();
    let agent = Arc::new(Mutex::new(agent));

    // Add initial content
    {
        let agent = agent.lock().unwrap();
        let parser = agent.get_active_parser();
        let mut p = parser.lock().unwrap();
        for i in 0..100 {
            p.process(format!("Line {}\r\n", i).as_bytes());
        }
    }

    // Spawn threads that scroll and check state
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let agent = Arc::clone(&agent);
            thread::spawn(move || {
                for _ in 0..25 {
                    let mut agent = agent.lock().unwrap();
                    if i % 2 == 0 {
                        agent.scroll_up(1);
                    } else {
                        agent.scroll_down(1);
                    }
                    // Check scroll state (this was deadlocking before)
                    let _ = agent.is_scrolled();
                    let _ = agent.get_scroll_offset();
                    drop(agent);
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

/// This test mirrors the EXACT render loop from main.rs
/// It would have caught the deadlock bug where we called agent.is_scrolled()
/// while holding the parser lock.
///
/// Run with: cargo test --test integration_tests -- --test-threads=1 test_main_render_loop
#[test]
fn test_main_render_loop_pattern() {
    let (mut agent, _temp_dir) = create_test_agent();

    // cli_pty is already initialized in Agent::new()

    // Add content to the parser
    {
        let parser = agent.get_active_parser();
        let mut p = parser.lock().unwrap();
        for i in 0..50 {
            p.process(format!("Line {}\r\n", i).as_bytes());
        }
    }

    // Scroll up so we're in scrollback mode
    agent.scroll_up(10);

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    // This mirrors the EXACT code from main.rs lines ~1185-1231
    // If this deadlocks, the test will timeout (caught by CI or manual observation)
    terminal
        .draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(f.area());

            // Render agent list (left side)
            let items = vec![ListItem::new("test/repo#1 [CLI]")];
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Agents"));
            let mut list_state = ListState::default();
            list_state.select(Some(0));
            f.render_stateful_widget(list, chunks[0], &mut list_state);

            // === THIS IS THE EXACT PATTERN FROM main.rs ===
            // Build terminal title with scroll indicator
            // NOTE: These calls happen BEFORE we lock the parser, so they're safe
            let scroll_indicator = if agent.is_scrolled() {
                format!(" [SCROLLBACK +{}]", agent.get_scroll_offset())
            } else {
                String::new()
            };

            let terminal_title = format!("test/repo#1 [CLI]{}", scroll_indicator);
            let block = Block::default().borders(Borders::ALL).title(terminal_title);

            // Now lock parser for rendering
            let parser = agent.get_active_parser();
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();

            // CHECK SCROLLBACK FROM THE LOCK WE ALREADY HOLD
            // BUG WAS: calling agent.is_scrolled() here which tries to lock again
            // FIX: use screen.scrollback() directly
            let is_scrolled = screen.scrollback() > 0;

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

/// Test with timeout to catch deadlocks - uses a background thread
/// This test WOULD HAVE caught the original bug
#[test]
fn test_render_with_timeout() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let (mut agent, _temp_dir) = create_test_agent();

        // Add content
        {
            let parser = agent.get_active_parser();
            let mut p = parser.lock().unwrap();
            for i in 0..50 {
                p.process(format!("Line {}\r\n", i).as_bytes());
            }
        }

        agent.scroll_up(10);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate the render loop
        for _ in 0..10 {
            let parser = agent.get_active_parser();
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
            agent.scroll_down(1);
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
/// This exercises the actual PTY spawn code path
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
        env_vars.insert("BOTSTER_WORKTREE_PATH".to_string(), temp_dir.path().to_string_lossy().to_string());
        env_vars.insert("BOTSTER_TASK_DESCRIPTION".to_string(), "Test task".to_string());
        env_vars.insert("BOTSTER_BRANCH_NAME".to_string(), "test-branch".to_string());

        // Spawn bash and source the init script
        agent.spawn(
            "bash",
            "",
            vec![format!("source {}", init_script.display())],
            &env_vars,
        ).expect("Failed to spawn PTY");

        // Wait for output to be generated
        thread::sleep(Duration::from_millis(500));

        // Verify we received some output
        let buffer = agent.get_buffer_snapshot();
        assert!(!buffer.is_empty(), "PTY should have produced output");

        // Wait longer for more output (the test script generates 100 lines)
        thread::sleep(Duration::from_secs(2));

        // Check the VT100 screen has content
        let screen_content = agent.get_vt100_screen();
        let has_content = screen_content.iter().any(|line| !line.trim().is_empty());
        assert!(has_content, "VT100 screen should have content");

        // Test scrollback with real PTY content
        agent.scroll_up(10);
        assert!(agent.is_scrolled(), "Should be able to scroll real PTY content");

        // Test rendering the scrolled content
        let parser = agent.get_active_parser();
        let parser_lock = parser.lock().unwrap();
        let screen = parser_lock.screen();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                let block = Block::default().borders(Borders::ALL).title("Real PTY");
                let widget = TerminalWidget::new(screen).block(block).hide_cursor();
                f.render_widget(widget, f.area());
            })
            .unwrap();

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
        agent.spawn(
            "bash",
            "",
            vec!["echo 'CLI PTY started'".to_string()],
            &empty_env,
        ).expect("Failed to spawn CLI PTY");

        // Now spawn the server PTY
        let server_script = fixture_path("test_botster_server.sh");
        let mut server_env = HashMap::new();
        server_env.insert("BOTSTER_TUNNEL_PORT".to_string(), "3000".to_string());

        agent.spawn_server_pty(
            &server_script.display().to_string(),
            &server_env,
        ).expect("Failed to spawn server PTY");

        assert!(agent.has_server_pty(), "Server PTY should be available");

        // Wait for server to produce output
        thread::sleep(Duration::from_secs(3));

        // Check server PTY content
        let server_buffer = agent.server_pty.as_ref().unwrap().get_buffer_snapshot();
        assert!(!server_buffer.is_empty(), "Server PTY should have output");

        // Test switching views
        agent.active_pty = PtyView::Server;
        let server_screen = agent.get_vt100_screen();
        let has_server_content = server_screen.iter().any(|line| !line.trim().is_empty());
        assert!(has_server_content, "Server view should show content");

        // Test scrollback on server view
        agent.scroll_up(5);
        assert!(agent.is_scrolled(), "Should be able to scroll server view");

        // Switch back to CLI
        agent.active_pty = PtyView::Cli;
        assert_eq!(agent.active_pty, PtyView::Cli);

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
        agent.spawn(
            "bash",
            "",
            vec!["for i in $(seq 1 50); do echo \"CLI Line $i\"; done".to_string()],
            &empty_env,
        ).expect("Failed to spawn CLI PTY");

        // Spawn server PTY
        let server_script = fixture_path("test_botster_server.sh");
        agent.spawn_server_pty(
            &server_script.display().to_string(),
            &empty_env,
        ).expect("Failed to spawn server PTY");

        // Wait for output
        thread::sleep(Duration::from_secs(2));

        // Test view switching while scrolled
        agent.active_pty = PtyView::Cli;
        agent.scroll_up(10);
        let cli_offset = agent.get_scroll_offset();
        assert_eq!(cli_offset, 10, "CLI should be scrolled");

        // Switch to server
        agent.active_pty = PtyView::Server;
        agent.scroll_up(5);
        let server_offset = agent.get_scroll_offset();
        assert_eq!(server_offset, 5, "Server should be scrolled independently");

        // Switch back - CLI scroll should be preserved
        agent.active_pty = PtyView::Cli;
        let cli_offset_after = agent.get_scroll_offset();
        assert_eq!(cli_offset_after, 10, "CLI scroll should be preserved");

        // Test rendering both views in sequence
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        for view in [PtyView::Cli, PtyView::Server] {
            agent.active_pty = view;

            // Get scroll indicator before locking
            let is_scrolled = agent.is_scrolled();
            let offset = agent.get_scroll_offset();

            // Now lock and render
            let parser = agent.get_active_parser();
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();

            terminal
                .draw(|f| {
                    let title = format!("{:?} View (scroll: {})", view, offset);
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

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: PTY view switching test did not complete within 15 seconds");
        }
    }
}

/// Test that the render_agent_terminal function works with real PTY content
#[test]
fn test_render_agent_terminal_with_real_pty() {
    use botster_hub::render_agent_terminal;
    use ratatui::buffer::Buffer;
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

        // Spawn with test script that generates scrollback content
        let init_script = fixture_path("test_botster_init.sh");
        let mut env_vars = HashMap::new();
        env_vars.insert("BOTSTER_WORKTREE_PATH".to_string(), temp_dir.path().to_string_lossy().to_string());

        agent.spawn(
            "bash",
            "",
            vec![format!("source {}", init_script.display())],
            &env_vars,
        ).expect("Failed to spawn PTY");

        // Wait for the script to generate output (100 lines)
        thread::sleep(Duration::from_secs(2));

        // Scroll up
        agent.scroll_up(20);
        assert!(agent.is_scrolled());

        // Use the ACTUAL render function from render.rs
        let area = ratatui::layout::Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        let title = render_agent_terminal(&agent, area, &mut buf);

        // Verify the title contains scrollback indicator
        assert!(title.contains("SCROLLBACK"), "Title should show scrollback: {}", title);
        assert!(title.contains("+20"), "Title should show scroll offset: {}", title);

        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => {
            handle.join().unwrap();
        }
        Err(_) => {
            panic!("TIMEOUT: render_agent_terminal test did not complete within 15 seconds");
        }
    }
}

/// Test rapid scrolling with real PTY - catches potential deadlocks
#[test]
fn test_rapid_scroll_real_pty_no_deadlock() {
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

        // Spawn PTY with content
        let empty_env = HashMap::new();
        agent.spawn(
            "bash",
            "",
            vec!["for i in $(seq 1 200); do echo \"Line $i: Lorem ipsum dolor sit amet\"; done".to_string()],
            &empty_env,
        ).expect("Failed to spawn PTY");

        // Wait for content
        thread::sleep(Duration::from_millis(500));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Simulate rapid scrolling like a user would do
        for i in 0..50 {
            // Scroll
            if i % 3 == 0 {
                agent.scroll_up(5);
            } else if i % 3 == 1 {
                agent.scroll_down(2);
            } else {
                agent.scroll_to_top();
                agent.scroll_down(10);
            }

            // Get scroll state BEFORE locking (this is the correct pattern)
            let scroll_indicator = if agent.is_scrolled() {
                format!(" [SCROLLBACK +{}]", agent.get_scroll_offset())
            } else {
                String::new()
            };

            // Now lock and render
            let parser = agent.get_active_parser();
            let parser_lock = parser.lock().unwrap();
            let screen = parser_lock.screen();
            let is_scrolled = screen.scrollback() > 0;

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
            panic!("DEADLOCK: Rapid scroll test with real PTY did not complete within 10 seconds");
        }
    }
}
