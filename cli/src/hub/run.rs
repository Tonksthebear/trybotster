//! Hub event loop implementation.
//!
//! Contains the headless run loop for Hub operations. TUI mode is now
//! handled by [`crate::tui::run_with_hub`] to maintain proper layer separation.
//!
//! # Architecture
//!
//! ## Event-Driven Design
//!
//! Both headless and TUI modes use `tokio::select!` to wait for events
//! instead of polling with `thread::sleep`. Hub is `!Send` (Lua VM), so
//! we use `Runtime::block_on()` which runs the future on the calling thread.
//!
//! ## Headless Mode (`run_headless_loop`)
//!
//! For CI/daemon use without a terminal. Hub processes commands and events
//! without any TUI rendering.
//!
//! ## TUI Mode
//!
//! See [`crate::tui::run_with_hub`] - the TUI module coordinates with Hub
//! via channels, with the Hub event loop also using `select!`.

// Rust guideline compliant 2026-02

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;

use crate::hub::Hub;

/// Run the Hub event loop without TUI (headless mode).
///
/// Fully event-driven via `tokio::select!`. Direct channel receivers
/// (PTY input, WebRTC signals, stream frames, worktree results) wake
/// the loop instantly. The unified `HubEvent` channel delivers all
/// background events (HTTP, WebSocket, timers, ActionCable, WebRTC,
/// PTY notifications, file watches, cleanup ticks) with zero latency.
/// No periodic polling — the loop sleeps between events.
///
/// # Arguments
///
/// * `hub` - The Hub instance to run
/// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
///
/// # Errors
///
/// Returns an error if the event loop encounters an unrecoverable failure.
pub fn run_headless_loop(hub: &mut Hub, shutdown_flag: &AtomicBool) -> Result<()> {
    log::info!("Hub event loop starting (headless, event-driven)");

    run_event_loop(hub, shutdown_flag, None)?;

    log::info!("Hub headless event loop exiting");
    Ok(())
}

/// Core event loop shared by headless and TUI modes.
///
/// Extracts channel receivers from Hub for `tokio::select!` and drives
/// the async loop via `Runtime::block_on()`. Hub is `!Send` (Lua VM), but
/// `block_on` runs the future on the calling thread — no Send required.
///
/// # Arguments
///
/// * `hub` - The Hub instance
/// * `shutdown_flag` - External shutdown signal (Ctrl+C)
/// * `tui_shutdown` - Optional TUI-initiated shutdown flag
///
/// # Errors
///
/// Returns an error if the event loop encounters an unrecoverable failure.
pub(crate) fn run_event_loop(
    hub: &mut Hub,
    shutdown_flag: &AtomicBool,
    tui_shutdown: Option<&AtomicBool>,
) -> Result<()> {
    // Extract receivers from Hub so select! can borrow them independently
    // of &mut hub in match arms. The poll_* fallback methods handle None
    // gracefully (early return).
    let mut pty_input_rx = hub.pty_input_rx.take();
    let mut webrtc_signal_rx = hub.webrtc_outgoing_signal_rx.take();
    let mut webrtc_pty_output_rx = hub.webrtc_pty_output_rx.take();
    let mut stream_frame_rx = hub.stream_frame_rx.take();
    let mut worktree_result_rx = hub.worktree_result_rx.take();
    let mut tui_request_rx = hub.tui_request_rx.take();
    let mut hub_event_rx = hub.hub_event_rx.take();

    // Spawn a cleanup interval task that sends CleanupTick every 5 seconds.
    // This replaces the periodic timer in the select! loop, allowing the
    // event loop to sleep fully between real events.
    let cleanup_tx = hub.hub_event_tx.clone();
    let cleanup_handle = hub.tokio_runtime.handle().spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if cleanup_tx.send(super::events::HubEvent::CleanupTick).is_err() {
                break; // Hub shut down
            }
        }
    });

    // Clone the runtime handle before entering async context.
    // block_on() drives the tokio reactor on the current (main) thread.
    // Hub is !Send (Lua VM), but block_on doesn't require Send — the future
    // executes entirely on this thread. Spawned tasks run on worker threads.
    let rt_handle = hub.tokio_runtime.handle().clone();
    rt_handle.block_on(async {
        loop {
            // select! with biased: check high-priority channels first
            tokio::select! {
                biased;

                // TUI requests (keyboard input, Lua messages)
                Some(req) = async {
                    match tui_request_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_tui_request(req);
                }

                // Binary PTY input from browser (zero-overhead keystroke path)
                Some(input) = async {
                    match pty_input_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_pty_input(input);
                    // Drain remaining in batch
                    if let Some(ref mut rx) = pty_input_rx {
                        while let Ok(more) = rx.try_recv() {
                            hub.handle_pty_input(more);
                        }
                    }
                }

                // WebRTC PTY output (highest volume — batch drain)
                Some(first) = async {
                    match webrtc_pty_output_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_webrtc_pty_output_batch(first, &mut webrtc_pty_output_rx);
                }

                // Outgoing WebRTC signals (ICE candidates)
                Some(signal) = async {
                    match webrtc_signal_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_webrtc_signal(signal);
                    if let Some(ref mut rx) = webrtc_signal_rx {
                        while let Ok(more) = rx.try_recv() {
                            hub.handle_webrtc_signal(more);
                        }
                    }
                }

                // Incoming stream frames (preview tunneling)
                Some(frame) = async {
                    match stream_frame_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_stream_frame(frame);
                    if let Some(ref mut rx) = stream_frame_rx {
                        while let Ok(more) = rx.try_recv() {
                            hub.handle_stream_frame(more);
                        }
                    }
                    // Drain multiplexer output inline.
                    hub.poll_stream_frames_outgoing();
                }

                // Worktree creation results
                Some(result) = async {
                    match worktree_result_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_worktree_result(result);
                }

                // Unified event bus (all events including cleanup ticks)
                Some(event) = async {
                    match hub_event_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_hub_event(event);
                }
            }

            // Flush Lua-queued operations after every event
            hub.flush_lua_queues();

            // Check shutdown conditions
            if hub.quit || shutdown_flag.load(Ordering::SeqCst) {
                break;
            }
            if let Some(flag) = tui_shutdown {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    });

    // Stop the cleanup interval task.
    cleanup_handle.abort();

    // Restore receivers for clean shutdown (Hub.drop may need them)
    hub.pty_input_rx = pty_input_rx;
    hub.webrtc_outgoing_signal_rx = webrtc_signal_rx;
    hub.webrtc_pty_output_rx = webrtc_pty_output_rx;
    hub.stream_frame_rx = stream_frame_rx;
    hub.worktree_result_rx = worktree_result_rx;
    hub.tui_request_rx = tui_request_rx;
    hub.hub_event_rx = hub_event_rx;

    Ok(())
}
