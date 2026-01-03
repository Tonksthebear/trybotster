//! Rendering utilities for the TUI
//!
//! This module contains rendering functions that can be tested directly,
//! ensuring the actual code paths are exercised by tests.

use crate::{Agent, PtyView, TerminalWidget};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::{Block, Borders},
};

/// Render an agent's terminal view to a buffer
///
/// This function encapsulates the terminal rendering logic from main.rs
/// so it can be tested directly. It handles:
/// - Building the terminal title with view and scroll indicators
/// - Locking the parser and getting the screen
/// - Creating and rendering the TerminalWidget
///
/// Returns the terminal title string for display.
pub fn render_agent_terminal(agent: &Agent, area: Rect, buf: &mut Buffer) -> String {
    // Build terminal title with view indicator
    let view_indicator = match agent.active_pty {
        PtyView::Cli => {
            if agent.has_server_pty() {
                "[CLI | Ctrl+T: Server]"
            } else {
                "[CLI]"
            }
        }
        PtyView::Server => "[SERVER | Ctrl+T: CLI]",
    };

    // Add scroll indicator if scrolled
    // IMPORTANT: These calls happen BEFORE we lock the parser
    let scroll_indicator = if agent.is_scrolled() {
        format!(
            " [SCROLLBACK +{} | Shift+End: live]",
            agent.get_scroll_offset()
        )
    } else {
        String::new()
    };

    let terminal_title = if let Some(issue_num) = agent.issue_number {
        format!(
            " {}#{} {}{} [Ctrl+P | Ctrl+J/K | Shift+PgUp/Dn scroll] ",
            agent.repo, issue_num, view_indicator, scroll_indicator
        )
    } else {
        format!(
            " {}/{} {}{} [Ctrl+P | Ctrl+J/K | Shift+PgUp/Dn scroll] ",
            agent.repo, agent.branch_name, view_indicator, scroll_indicator
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(terminal_title.clone());

    // Lock parser for rendering
    let parser = agent.get_active_parser();
    let parser_lock = parser.lock().unwrap();
    let screen = parser_lock.screen();

    // CRITICAL: Check scrollback from the lock we already hold
    // DO NOT call agent.is_scrolled() here - it would deadlock!
    let is_scrolled = screen.scrollback() > 0;

    let widget = if is_scrolled {
        TerminalWidget::new(screen).block(block).hide_cursor()
    } else {
        TerminalWidget::new(screen).block(block)
    };

    // Render directly to buffer
    use ratatui::widgets::Widget;
    widget.render(area, buf);

    terminal_title
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PtySession;
    use ratatui::buffer::Buffer;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

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

    /// Test that render_agent_terminal doesn't deadlock
    /// This tests the ACTUAL code path, not a copy
    #[test]
    fn test_render_agent_terminal_no_deadlock() {
        let (mut agent, _temp_dir) = create_test_agent();
        agent.cli_pty = Some(PtySession::new(24, 80));

        // Add content
        {
            let parser = agent.get_active_parser();
            let mut p = parser.lock().unwrap();
            for i in 0..50 {
                p.process(format!("Line {}\r\n", i).as_bytes());
            }
        }

        // Scroll up
        agent.scroll_up(10);
        assert!(agent.is_scrolled());

        // Render using the actual function
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let title = render_agent_terminal(&agent, area, &mut buf);

        assert!(title.contains("SCROLLBACK"));
        assert!(title.contains("+10"));
    }

    /// Test with timeout to catch deadlocks
    #[test]
    fn test_render_agent_terminal_with_timeout() {
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut agent, _temp_dir) = create_test_agent();
            agent.cli_pty = Some(PtySession::new(24, 80));

            // Add content
            {
                let parser = agent.get_active_parser();
                let mut p = parser.lock().unwrap();
                for i in 0..50 {
                    p.process(format!("Line {}\r\n", i).as_bytes());
                }
            }

            // Repeatedly scroll and render - tests the actual code path
            for i in 0..20 {
                if i % 2 == 0 {
                    agent.scroll_up(5);
                } else {
                    agent.scroll_down(3);
                }

                let area = Rect::new(0, 0, 80, 24);
                let mut buf = Buffer::empty(area);
                let _ = render_agent_terminal(&agent, area, &mut buf);
            }

            tx.send(()).unwrap();
        });

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {
                handle.join().unwrap();
            }
            Err(_) => {
                panic!("DEADLOCK: render_agent_terminal did not complete within 5 seconds");
            }
        }
    }

    /// Test switching between PTY views and rendering
    #[test]
    fn test_render_with_pty_switch() {
        let (mut agent, _temp_dir) = create_test_agent();
        agent.cli_pty = Some(PtySession::new(24, 80));
        agent.server_pty = Some(PtySession::new(24, 80));

        // Add content to both
        {
            let mut p = agent
                .cli_pty
                .as_ref()
                .unwrap()
                .vt100_parser
                .lock()
                .unwrap();
            for i in 0..30 {
                p.process(format!("CLI {}\r\n", i).as_bytes());
            }
        }
        {
            let mut p = agent
                .server_pty
                .as_ref()
                .unwrap()
                .vt100_parser
                .lock()
                .unwrap();
            for i in 0..30 {
                p.process(format!("SERVER {}\r\n", i).as_bytes());
            }
        }

        let area = Rect::new(0, 0, 80, 24);

        // Render CLI view
        agent.active_pty = PtyView::Cli;
        agent.scroll_up(5);
        let mut buf = Buffer::empty(area);
        let title = render_agent_terminal(&agent, area, &mut buf);
        assert!(title.contains("[CLI"));

        // Switch to server and render
        agent.active_pty = PtyView::Server;
        let mut buf = Buffer::empty(area);
        let title = render_agent_terminal(&agent, area, &mut buf);
        assert!(title.contains("[SERVER"));

        // Switch back - CLI scroll should be preserved
        agent.active_pty = PtyView::Cli;
        assert_eq!(agent.get_scroll_offset(), 5);
    }
}
