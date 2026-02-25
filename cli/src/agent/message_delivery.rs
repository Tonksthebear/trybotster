//! Message delivery system for PTY sessions.
//!
//! Provides reliable message injection into agent PTYs with a probe-based
//! delivery gate. Messages are queued and delivered when the PTY is in a
//! state that accepts free-text input.
//!
//! # Probe Mechanism
//!
//! Before delivering a message, the system injects a two-character probe
//! sequence (`zx`) into the PTY and watches for it to echo back in the
//! output stream. If the probe echoes, the PTY is accepting free-text
//! input — the probe is erased with backspaces and the message is
//! delivered. If no echo within 200ms, the PTY is in a modal state
//! (permission prompt, multi-choice, etc.) and delivery is retried on
//! the next output event.
//!
//! # Human Activity Detection
//!
//! Delivery is deferred when a human is actively typing (determined by
//! a timestamp updated on every PTY input event from a focused client).
//! This prevents message injection from interrupting manual interaction.

// Rust guideline compliant 2026-02

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, Notify};

use super::pty::events::PtyEvent;
use super::pty::SharedPtyState;

/// Probe sequence injected to test if the PTY accepts free-text input.
const PROBE: &[u8] = b"zx";

/// Backspace in legacy mode (Ctrl+H).
const BACKSPACE_LEGACY: &[u8] = b"\x08";

/// Backspace in kitty keyboard protocol (CSI 127 u).
const BACKSPACE_KITTY: &[u8] = b"\x1b[127u";

/// Enter key (carriage return). Always legacy encoding — even with kitty's
/// DISAMBIGUATE_ESCAPE_CODES flag, unmodified Enter is still raw CR.
const ENTER: &[u8] = b"\r";

/// Bracketed paste start (CSI 200 ~).
///
/// Wrapping message content in bracketed paste prevents the terminal app
/// from interpreting embedded newlines as individual Enter keypresses.
const PASTE_START: &[u8] = b"\x1b[200~";

/// Bracketed paste end (CSI 201 ~).
const PASTE_END: &[u8] = b"\x1b[201~";

/// Maximum time to wait for probe echo before retrying.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Minimum interval since last human input before attempting delivery.
const HUMAN_ACTIVITY_COOLDOWN: Duration = Duration::from_secs(2);

/// Shared state for message delivery on a single PTY session.
///
/// Created lazily on the first `send_message()` call. The delivery task
/// runs as a tokio task and communicates with the Lua-facing handle
/// through this shared state.
pub struct MessageDeliveryState {
    /// Pending messages waiting for delivery.
    queue: Mutex<VecDeque<String>>,

    /// Wakes the delivery task when a new message is queued
    /// or when PTY output arrives (potential retry trigger).
    wake: Notify,
}

impl std::fmt::Debug for MessageDeliveryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let q = self.queue.lock().expect("delivery queue lock poisoned");
        f.debug_struct("MessageDeliveryState")
            .field("pending", &q.len())
            .finish()
    }
}

impl MessageDeliveryState {
    /// Create a new delivery state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            wake: Notify::new(),
        }
    }

    /// Queue a message for delivery.
    pub fn enqueue(&self, message: String) {
        {
            let mut q = self.queue.lock().expect("delivery queue lock poisoned");
            q.push_back(message);
        }
        self.wake.notify_one();
    }

    /// Take the next pending message from the queue.
    fn dequeue(&self) -> Option<String> {
        let mut q = self.queue.lock().expect("delivery queue lock poisoned");
        q.pop_front()
    }

    /// Check if the queue has pending messages.
    fn has_pending(&self) -> bool {
        let q = self.queue.lock().expect("delivery queue lock poisoned");
        !q.is_empty()
    }

}

/// Check if a human was recently active on this PTY.
///
/// Reads the `last_human_input_ms` atomic directly (no mutex needed).
/// The timestamp is stamped by `write_input_direct()` on every human keystroke.
fn is_human_active(last_human_input_ms: &std::sync::atomic::AtomicI64) -> bool {
    let last = last_human_input_ms.load(Ordering::Relaxed);
    if last == 0 {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    (now - last) < HUMAN_ACTIVITY_COOLDOWN.as_millis() as i64
}

/// Write bytes to the PTY via shared state.
fn write_to_pty(shared_state: &Arc<Mutex<SharedPtyState>>, data: &[u8]) -> bool {
    let mut state = match shared_state.lock() {
        Ok(s) => s,
        Err(_) => return false,
    };
    if let Some(writer) = &mut state.writer {
        use std::io::Write;
        if writer.write_all(data).is_ok() {
            let _ = writer.flush();
            return true;
        }
    }
    false
}

/// Check if raw PTY output bytes contain the probe echo.
fn contains_probe_echo(data: &[u8]) -> bool {
    data.windows(PROBE.len()).any(|w| w == PROBE)
}

/// Spawn the delivery task for a PTY session.
///
/// The task runs until the PTY broadcast channel closes (process exit).
/// It watches for queued messages, probes the PTY, and delivers when
/// the PTY is in a free-text input state.
pub(crate) fn spawn_delivery_task(
    delivery: Arc<MessageDeliveryState>,
    shared_state: Arc<Mutex<SharedPtyState>>,
    event_tx: broadcast::Sender<PtyEvent>,
    hub_event_tx: Option<mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
    kitty_enabled: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    // Clone the atomic timestamp once — read directly without locking SharedPtyState.
    let human_input_ts = {
        let state = shared_state.lock().expect("shared_state lock poisoned");
        Arc::clone(&state.last_human_input_ms)
    };

    tokio::spawn(async move {
        log::info!("[MessageDelivery] Delivery task started");

        loop {
            // Wait for a message to be queued or a wake signal.
            delivery.wake.notified().await;

            // Process all pending messages.
            while delivery.has_pending() {
                // Wait out human activity cooldown.
                while is_human_active(&human_input_ts) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }

                // Attempt probe-based delivery.
                match attempt_delivery(&delivery, &shared_state, &event_tx, &human_input_ts, &kitty_enabled).await {
                    DeliveryResult::Delivered(msg) => {
                        log::info!("[MessageDelivery] Message delivered ({} bytes)", msg.len());
                        // Notify via hub event if available.
                        if let Some(tx) = &hub_event_tx {
                            let _ = tx.send(crate::hub::events::HubEvent::MessageDelivered {
                                message_len: msg.len(),
                            });
                        }
                    }
                    DeliveryResult::PtyUnavailable => {
                        log::warn!("[MessageDelivery] PTY write unavailable, stopping");
                        return;
                    }
                    DeliveryResult::ChannelClosed => {
                        log::info!("[MessageDelivery] PTY channel closed, stopping");
                        return;
                    }
                }
            }
        }
    })
}

/// Result of a single delivery attempt.
enum DeliveryResult {
    /// Message was successfully delivered.
    Delivered(String),
    /// PTY writer is gone (session ended).
    PtyUnavailable,
    /// Broadcast channel closed (process exited).
    ChannelClosed,
}

/// Attempt to deliver the next message using the probe mechanism.
///
/// 1. Inject `zx` probe
/// 2. Watch PTY output for echo within 200ms
/// 3. If echo: erase probe, deliver message
/// 4. If no echo: wait for more output, retry
async fn attempt_delivery(
    delivery: &Arc<MessageDeliveryState>,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
    human_input_ts: &std::sync::atomic::AtomicI64,
    kitty_enabled: &AtomicBool,
) -> DeliveryResult {
    let mut rx = event_tx.subscribe();

    // Retry loop: keep probing until we succeed or the PTY dies.
    loop {
        // Re-check human activity before each probe attempt.
        if is_human_active(human_input_ts) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        // Inject probe.
        if !write_to_pty(shared_state, PROBE) {
            return DeliveryResult::PtyUnavailable;
        }

        // Watch for probe echo in PTY output.
        let echo_detected = tokio::time::timeout(PROBE_TIMEOUT, async {
            loop {
                match rx.recv().await {
                    Ok(PtyEvent::Output(data)) => {
                        if contains_probe_echo(&data) {
                            return true;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return false;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Missed some events, continue watching.
                        continue;
                    }
                    _ => continue,
                }
            }
        })
        .await;

        match echo_detected {
            Ok(true) => {
                // Probe echoed — PTY is accepting input.
                // Erase probe and deliver message.
                let msg = match delivery.dequeue() {
                    Some(m) => m,
                    None => return DeliveryResult::Delivered(String::new()),
                };

                // Three-phase delivery with kitty-aware key encodings:
                //   1. Erase probe (backspace × 2)
                //   2. Paste message (bracketed paste)
                //   3. Submit (Enter)
                // Each phase is a separate write with a small delay so the
                // app processes each action before receiving the next.
                let kitty = kitty_enabled.load(Ordering::Relaxed);
                let bs: &[u8] = if kitty { BACKSPACE_KITTY } else { BACKSPACE_LEGACY };

                // Phase 1: Erase probe characters one at a time.
                // Claude Code processes one key event per frame, so each
                // backspace needs its own write + delay.
                for _ in 0..PROBE.len() {
                    if !write_to_pty(shared_state, bs) {
                        return DeliveryResult::PtyUnavailable;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }

                // Phase 2: Deliver message as bracketed paste.
                let mut paste = Vec::new();
                paste.extend_from_slice(PASTE_START);
                paste.extend_from_slice(msg.as_bytes());
                paste.extend_from_slice(PASTE_END);
                if !write_to_pty(shared_state, &paste) {
                    return DeliveryResult::PtyUnavailable;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;

                // Phase 3: Submit with Enter.
                // Always use legacy \r — even with kitty DISAMBIGUATE_ESCAPE_CODES,
                // unmodified Enter is still sent as raw CR by terminals.
                if !write_to_pty(shared_state, ENTER) {
                    return DeliveryResult::PtyUnavailable;
                }

                log::info!(
                    "[MessageDelivery] Delivered {} bytes (kitty={}) in 3 phases",
                    msg.len(), kitty
                );
                return DeliveryResult::Delivered(msg);
            }
            Ok(false) => {
                // Channel closed.
                return DeliveryResult::ChannelClosed;
            }
            Err(_timeout) => {
                // No echo within timeout — PTY is in a modal state.
                // Erase the probe (it may have been consumed silently)
                // and wait for more PTY output before retrying.
                let probe_bs: &[u8] = if kitty_enabled.load(Ordering::Relaxed) {
                    BACKSPACE_KITTY
                } else {
                    BACKSPACE_LEGACY
                };
                for _ in 0..PROBE.len() {
                    let _ = write_to_pty(shared_state, probe_bs);
                }

                log::debug!("[MessageDelivery] Probe timeout, waiting for PTY output to retry");

                // Wait for next PTY output event or a new wake signal.
                tokio::select! {
                    result = rx.recv() => {
                        match result {
                            Err(broadcast::error::RecvError::Closed) => {
                                return DeliveryResult::ChannelClosed;
                            }
                            _ => continue, // Retry probe on next output.
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        // Periodic retry even without output.
                        continue;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contains_probe_echo() {
        assert!(contains_probe_echo(b"some output zx more output"));
        assert!(contains_probe_echo(b"zx"));
        assert!(contains_probe_echo(b"\x1b[32mzx\x1b[0m"));
        assert!(!contains_probe_echo(b"some output without probe"));
        assert!(!contains_probe_echo(b"xz")); // reversed
    }

    #[test]
    fn test_human_activity_detection() {
        use std::sync::atomic::AtomicI64;

        let ts = AtomicI64::new(0);
        assert!(!is_human_active(&ts));

        // Stamp current time.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        ts.store(now, Ordering::Relaxed);
        assert!(is_human_active(&ts));

        // Stamp a time well in the past (beyond cooldown).
        ts.store(now - 10_000, Ordering::Relaxed);
        assert!(!is_human_active(&ts));
    }

    #[test]
    fn test_message_queue() {
        let state = MessageDeliveryState::new();

        assert!(!state.has_pending());
        assert!(state.dequeue().is_none());

        state.enqueue("hello".to_string());
        state.enqueue("world".to_string());

        assert!(state.has_pending());
        assert_eq!(state.dequeue(), Some("hello".to_string()));
        assert_eq!(state.dequeue(), Some("world".to_string()));
        assert!(!state.has_pending());
    }
}
