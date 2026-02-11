//! Hub event loop implementation.
//!
//! Contains the headless run loop for Hub operations. TUI mode is now
//! handled by [`crate::tui::run_with_hub`] to maintain proper layer separation.
//!
//! # Architecture
//!
//! ## Headless Mode (run_headless_loop)
//!
//! For CI/daemon use without a terminal. Hub processes commands and events
//! without any TUI rendering.
//!
//! ## TUI Mode
//!
//! See [`crate::tui::run_with_hub`] - the TUI module now owns TuiRunner
//! instantiation and coordinates with Hub via channels.

// Rust guideline compliant 2026-01

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::hub::Hub;

/// Run the Hub event loop without TUI (headless mode).
///
/// Used for CI, daemon mode, or when no terminal is available.
/// Processes commands, browser events, and periodic tasks without rendering.
///
/// # Arguments
///
/// * `hub` - The Hub instance to run
/// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
///
/// # Errors
///
/// Returns an error if operation fails.
pub fn run_headless_loop(hub: &mut Hub, shutdown_flag: &AtomicBool) -> Result<()> {
    log::info!("Hub event loop starting (headless mode)");

    while !hub.quit && !shutdown_flag.load(Ordering::SeqCst) {
        // Periodic tasks (command channel, heartbeat, Lua queues, etc.)
        hub.tick();

        // Sleep to prevent CPU spinning
        thread::sleep(Duration::from_millis(16));
    }

    log::info!("Hub headless event loop exiting");
    Ok(())
}
