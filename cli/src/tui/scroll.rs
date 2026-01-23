//! Scroll management for terminal views.
//!
//! Provides scroll operations for VT100 parser screens. These functions
//! operate on a parser reference, handling scrollback buffer navigation.
//!
//! # Design
//!
//! Functions in this module take a parser reference (Arc<Mutex<Parser>>).
//! Each client (TuiClient, TuiRunner) owns their own parser and manages
//! their own scroll state.
//!
//! # Client State
//!
//! Scroll position is stored in the VT100 parser (`screen.scrollback()`).
//! Since each client owns their own parser, scroll positions are independent.

// Rust guideline compliant 2026-01

use std::sync::{Arc, Mutex};
use vt100::Parser;

/// Check if we're in scrollback mode (scrolled up from live view).
#[must_use]
pub fn is_scrolled_parser(parser: &Arc<Mutex<Parser>>) -> bool {
    let p = parser.lock().expect("parser lock poisoned");
    p.screen().scrollback() > 0
}

/// Get current scroll offset from parser.
#[must_use]
pub fn get_offset_parser(parser: &Arc<Mutex<Parser>>) -> usize {
    let p = parser.lock().expect("parser lock poisoned");
    p.screen().scrollback()
}

/// Scroll up by the specified number of lines.
pub fn up_parser(parser: &Arc<Mutex<Parser>>, lines: usize) {
    let mut p = parser.lock().expect("parser lock poisoned");
    let current = p.screen().scrollback();
    p.screen_mut().set_scrollback(current.saturating_add(lines));
}

/// Scroll down by the specified number of lines.
pub fn down_parser(parser: &Arc<Mutex<Parser>>, lines: usize) {
    let mut p = parser.lock().expect("parser lock poisoned");
    let current = p.screen().scrollback();
    p.screen_mut().set_scrollback(current.saturating_sub(lines));
}

/// Scroll to the bottom (return to live view).
pub fn to_bottom_parser(parser: &Arc<Mutex<Parser>>) {
    let mut p = parser.lock().expect("parser lock poisoned");
    p.screen_mut().set_scrollback(0);
}

/// Scroll to the top of the scrollback buffer.
pub fn to_top_parser(parser: &Arc<Mutex<Parser>>) {
    let mut p = parser.lock().expect("parser lock poisoned");
    p.screen_mut().set_scrollback(usize::MAX);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_parser() -> Arc<Mutex<Parser>> {
        Arc::new(Mutex::new(Parser::new(24, 80, 1000)))
    }

    #[test]
    fn test_initial_scroll_state() {
        let parser = create_test_parser();

        assert!(!is_scrolled_parser(&parser));
        assert_eq!(get_offset_parser(&parser), 0);
    }

    #[test]
    fn test_scroll_up_and_down() {
        let parser = create_test_parser();

        // Add content to enable scrollback
        {
            let mut p = parser.lock().unwrap();
            for i in 0..50 {
                p.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        up_parser(&parser, 10);
        assert!(is_scrolled_parser(&parser));
        assert_eq!(get_offset_parser(&parser), 10);

        down_parser(&parser, 5);
        assert_eq!(get_offset_parser(&parser), 5);

        to_bottom_parser(&parser);
        assert!(!is_scrolled_parser(&parser));
        assert_eq!(get_offset_parser(&parser), 0);
    }

    #[test]
    fn test_scroll_to_top() {
        let parser = create_test_parser();

        // Add content
        {
            let mut p = parser.lock().unwrap();
            for i in 0..100 {
                p.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        to_top_parser(&parser);
        assert!(is_scrolled_parser(&parser));
        let offset = get_offset_parser(&parser);
        assert!(offset > 0);
    }

    #[test]
    fn test_scroll_down_does_not_go_negative() {
        let parser = create_test_parser();

        down_parser(&parser, 100);
        assert_eq!(get_offset_parser(&parser), 0);
    }

    #[test]
    fn test_scroll_independence_between_parsers() {
        let cli_parser = create_test_parser();
        let server_parser = create_test_parser();

        // Add content to both parsers
        {
            let mut p = cli_parser.lock().unwrap();
            for i in 0..50 {
                p.process(format!("CLI Line {i}\r\n").as_bytes());
            }
        }
        {
            let mut p = server_parser.lock().unwrap();
            for i in 0..30 {
                p.process(format!("Server Line {i}\r\n").as_bytes());
            }
        }

        // Scroll CLI parser
        up_parser(&cli_parser, 15);
        assert_eq!(get_offset_parser(&cli_parser), 15);

        // Server parser should be unaffected
        assert_eq!(get_offset_parser(&server_parser), 0);

        // Scroll Server parser independently
        up_parser(&server_parser, 5);
        assert_eq!(get_offset_parser(&server_parser), 5);

        // CLI parser should still have its scroll position
        assert_eq!(get_offset_parser(&cli_parser), 15);
    }

    #[test]
    fn test_scroll_up_extreme_value() {
        let parser = create_test_parser();

        {
            let mut p = parser.lock().unwrap();
            for i in 0..50 {
                p.process(format!("Line {i}\r\n").as_bytes());
            }
        }

        // Should not crash with extreme values
        up_parser(&parser, usize::MAX);
        assert!(is_scrolled_parser(&parser));
    }
}
