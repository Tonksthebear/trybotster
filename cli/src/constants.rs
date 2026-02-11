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
//! - **Server**: API and heartbeat configuration

use std::time::Duration;

// ============================================================================
// Identity
// ============================================================================

/// User-Agent header sent with all API requests to the Rails server.
///
/// Includes the CLI version so Rails can track which versions are in the wild
/// and gate compatibility if needed.
pub fn user_agent() -> String {
    format!("botster-hub/{}", crate::commands::update::VERSION)
}

// ============================================================================
// Timeouts
// ============================================================================

/// HTTP client request timeout for API calls.
///
/// This timeout applies to individual HTTP requests to the server API.
/// 10 seconds is sufficient for most API operations while preventing
/// indefinite hangs on network issues.
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

// ============================================================================
// Polling & Heartbeat
// ============================================================================

/// Heartbeat interval for WebRTC connections.
///
/// The server expects regular heartbeats to maintain connection state.
/// 30 seconds provides a balance between connection freshness and
/// network overhead.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// TUI frame rate delay (approximately 60fps).
///
/// Controls how often the TUI redraws. 16ms gives roughly 60fps
/// which provides smooth visual updates without excessive CPU usage.
pub const FRAME_RATE_DELAY: Duration = Duration::from_millis(16);

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
}
