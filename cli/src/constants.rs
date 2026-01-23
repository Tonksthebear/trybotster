//! Application-wide constants for botster-hub.
//!
//! This module centralizes all magic numbers and configuration constants
//! to improve maintainability and discoverability. Constants are grouped
//! by domain with documentation explaining their purpose.
//!
//! # Categories
//!
//! - **Timeouts**: Network and operation timeouts
//! - **Polling**: Event loop and background task intervals
//! - **UI**: Layout percentages and dimensions
//! - **Server**: API and heartbeat configuration

use std::time::Duration;

// ============================================================================
// Timeouts
// ============================================================================

/// HTTP client request timeout for API calls.
///
/// This timeout applies to individual HTTP requests to the server API.
/// 10 seconds is sufficient for most API operations while preventing
/// indefinite hangs on network issues.
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// WebRTC operation timeout.
///
/// Used for blocking operations in WebRTC handlers to prevent the TUI
/// from freezing if the server becomes unresponsive.
pub const WEBRTC_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Retry delay after failed spawn operations.
///
/// When spawning an agent fails, this delay prevents rapid retry loops
/// that could overwhelm resources.
pub const SPAWN_RETRY_DELAY: Duration = Duration::from_secs(5);

// ============================================================================
// Polling & Heartbeat
// ============================================================================

/// Heartbeat interval for WebRTC connections.
///
/// The server expects regular heartbeats to maintain connection state.
/// 30 seconds provides a balance between connection freshness and
/// network overhead.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Initial heartbeat offset to trigger immediate first heartbeat.
///
/// When an agent connects, subtracting this from the current time
/// ensures the first heartbeat is sent immediately rather than
/// waiting for the full interval.
pub const HEARTBEAT_INITIAL_OFFSET: Duration = Duration::from_secs(60);

/// TUI frame rate delay (approximately 60fps).
///
/// Controls how often the TUI redraws. 16ms gives roughly 60fps
/// which provides smooth visual updates without excessive CPU usage.
pub const FRAME_RATE_DELAY: Duration = Duration::from_millis(16);

/// Short poll interval for tight event loops.
///
/// Used when waiting for responses or processing events quickly.
/// 50ms provides responsive feedback without spinning the CPU.
pub const POLL_INTERVAL_SHORT: Duration = Duration::from_millis(50);

/// Medium poll interval for background operations.
///
/// Used for less time-critical polling operations.
pub const POLL_INTERVAL_MEDIUM: Duration = Duration::from_millis(100);

/// Startup delay after spawning processes.
///
/// Gives processes time to initialize before checking their status.
pub const PROCESS_STARTUP_DELAY: Duration = Duration::from_millis(500);

/// Very short poll interval for rapid polling.
///
/// Used when maximum responsiveness is needed, such as during
/// active data streaming.
pub const POLL_INTERVAL_RAPID: Duration = Duration::from_millis(5);

/// Tiny poll interval for blocking receivers.
///
/// Used with channel receivers to avoid blocking indefinitely
/// while still allowing quick response to incoming data.
pub const POLL_INTERVAL_MICRO: Duration = Duration::from_millis(10);

// ============================================================================
// UI Layout
// ============================================================================

/// Percentage of screen width for the agent list panel.
pub const AGENT_LIST_WIDTH_PERCENT: u16 = 30;

/// Percentage of screen width for the terminal panel.
pub const TERMINAL_WIDTH_PERCENT: u16 = 70;

/// Percentage of terminal width used for agent PTY calculation.
///
/// When calculating PTY size from browser dimensions, we use 70%
/// of the available width to account for borders and padding.
pub const PTY_WIDTH_PERCENT: u32 = 70;

/// Modal dialog width for menu popups.
pub const MENU_MODAL_WIDTH_PERCENT: u16 = 50;

/// Modal dialog height for menu popups.
pub const MENU_MODAL_HEIGHT_PERCENT: u16 = 30;

/// Modal dialog width for issue selection.
pub const ISSUE_MODAL_WIDTH_PERCENT: u16 = 70;

/// Modal dialog height for issue selection.
pub const ISSUE_MODAL_HEIGHT_PERCENT: u16 = 50;

/// Modal dialog width for branch input.
pub const BRANCH_INPUT_MODAL_WIDTH_PERCENT: u16 = 60;

/// Modal dialog height for branch input.
pub const BRANCH_INPUT_MODAL_HEIGHT_PERCENT: u16 = 30;

/// Modal dialog width for confirmation dialogs.
pub const CONFIRM_MODAL_WIDTH_PERCENT: u16 = 50;

/// Modal dialog height for confirmation dialogs.
pub const CONFIRM_MODAL_HEIGHT_PERCENT: u16 = 20;

// ============================================================================
// Browser/WebRTC
// ============================================================================

/// Minimum browser columns for a valid resize.
///
/// Below this threshold, resize requests are ignored to prevent
/// unusable terminal sizes.
pub const MIN_BROWSER_COLS: u16 = 20;

/// Minimum browser rows for a valid resize.
///
/// Below this threshold, resize requests are ignored to prevent
/// unusable terminal sizes.
pub const MIN_BROWSER_ROWS: u16 = 5;

/// Border padding to subtract from calculated PTY dimensions.
///
/// Accounts for UI borders when calculating the effective PTY size
/// from browser dimensions.
pub const PTY_BORDER_PADDING: u16 = 2;

// ============================================================================
// Menu Items
// ============================================================================

/// Menu item labels for the main menu popup.
pub const MENU_ITEMS: &[&str] = &[
    "Toggle Polling",
    "New Agent",
    "Close Agent",
    "Show Connection Code",
];

/// Index for "Toggle Polling" menu item.
pub const MENU_INDEX_TOGGLE_POLLING: usize = 0;

/// Index for "New Agent" menu item.
pub const MENU_INDEX_NEW_AGENT: usize = 1;

/// Index for "Close Agent" menu item.
pub const MENU_INDEX_CLOSE_AGENT: usize = 2;

/// Index for "Show Connection Code" menu item.
pub const MENU_INDEX_CONNECTION_CODE: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_values_are_reasonable() {
        // HTTP timeout should be between 5-60 seconds
        assert!(HTTP_REQUEST_TIMEOUT >= Duration::from_secs(5));
        assert!(HTTP_REQUEST_TIMEOUT <= Duration::from_secs(60));

        // Heartbeat should be at least 10 seconds
        assert!(HEARTBEAT_INTERVAL >= Duration::from_secs(10));
    }

    #[test]
    fn test_ui_percentages_sum_correctly() {
        // Agent list + terminal should equal 100%
        assert_eq!(AGENT_LIST_WIDTH_PERCENT + TERMINAL_WIDTH_PERCENT, 100);
    }

    #[test]
    fn test_menu_items_count_matches_indices() {
        // Ensure all menu indices are valid
        assert!(MENU_INDEX_TOGGLE_POLLING < MENU_ITEMS.len());
        assert!(MENU_INDEX_NEW_AGENT < MENU_ITEMS.len());
        assert!(MENU_INDEX_CLOSE_AGENT < MENU_ITEMS.len());
        assert!(MENU_INDEX_CONNECTION_CODE < MENU_ITEMS.len());
    }

    #[test]
    fn test_poll_intervals_ordering() {
        // Poll intervals should be in ascending order
        // RAPID (5ms) < MICRO (10ms) < SHORT (50ms) < MEDIUM (100ms)
        assert!(POLL_INTERVAL_RAPID < POLL_INTERVAL_MICRO);
        assert!(POLL_INTERVAL_MICRO < POLL_INTERVAL_SHORT);
        assert!(POLL_INTERVAL_SHORT < POLL_INTERVAL_MEDIUM);
    }

    #[test]
    fn test_browser_minimums_are_valid() {
        // Minimum dimensions should be positive and reasonable
        assert!(MIN_BROWSER_COLS >= 10);
        assert!(MIN_BROWSER_ROWS >= 3);
    }
}
