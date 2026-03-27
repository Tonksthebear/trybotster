//! Hub-side connection to a per-session process.
//!
//! Each `SessionConnection` owns a Unix socket stream to one session process.
//! After `install_reader()`, a dedicated thread reads all frames from the socket:
//!
//! - `PtyOutput` → broadcast as `PtyEvent::Output` (no shadow screen)
//! - Structured events (0x10-0x15) → mapped to `PtyEvent` variants, atomics updated
//! - `ProcessExited` → sent as `HubEvent::SessionProcessExited`
//! - Control responses (Snapshot, Screen, ModeFlags, Pong) → routed to `response_rx`
//!
//! RPCs (get_snapshot, get_screen, etc.) send their request on the write stream
//! and receive the response via `response_rx`. No socket read contention.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use tokio::sync::broadcast;

use crate::agent::notification::AgentNotification;
use crate::agent::pty::{PromptMark, PtyEvent};

use super::protocol::*;
use super::SpawnConfig;

/// Response timeout for RPCs after reader is installed.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared session connection for concurrent access from PtyHandle.
pub type SharedSessionConnection = Arc<Mutex<Option<SessionConnection>>>;

/// Hub-side connection to a single session process.
pub struct SessionConnection {
    stream: UnixStream,
    /// Pre-reader: frame decoder for direct socket reads.
    /// Post-reader: unused (reader thread owns decoding).
    decoder: FrameDecoder,
    /// Post-reader: RPC responses arrive here.
    response_rx: Option<std::sync::mpsc::Receiver<Frame>>,
    /// Whether the reader thread is alive.
    reader_alive: Arc<AtomicBool>,
    /// Protocol version negotiated during handshake.
    pub protocol_version: u8,
    /// Session metadata received during handshake.
    pub metadata: SessionMetadata,
}

impl std::fmt::Debug for SessionConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConnection")
            .field("protocol_version", &self.protocol_version)
            .field("metadata", &self.metadata)
            .field("reader_alive", &self.reader_alive)
            .finish_non_exhaustive()
    }
}

impl SessionConnection {
    /// Connect to a session process socket and perform handshake.
    pub fn connect(socket_path: &Path) -> Result<Self> {
        let mut stream = UnixStream::connect(socket_path)
            .with_context(|| format!("connect to session: {}", socket_path.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set session socket read timeout")?;

        let (version, metadata) = handshake_hub(&mut stream).context("session handshake")?;

        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            response_rx: None,
            reader_alive: Arc::new(AtomicBool::new(false)),
            protocol_version: version,
            metadata,
        })
    }

    /// Send spawn configuration to the session process.
    pub fn send_spawn_config(&mut self, config: &SpawnConfig) -> Result<()> {
        let frame = encode_json(FRAME_PTY_INPUT, config)?;
        self.stream.write_all(&frame).context("send spawn config")?;
        self.stream.flush().context("flush spawn config")?;
        Ok(())
    }

    /// Install the reader thread.
    ///
    /// Spawns a background thread that reads all frames from a dup of the
    /// session socket and routes them:
    ///
    /// - `PtyOutput` → broadcasts `PtyEvent::Output` (no shadow screen parsing)
    /// - Structured events (0x10-0x15) → mapped to `PtyEvent` variants
    /// - `ProcessExited` → `hub_event_tx` as `SessionProcessExited`
    /// - Control responses → `response_rx` for RPC callers
    ///
    /// After this call, `read_response()` uses the channel instead of
    /// reading the socket directly.
    pub(crate) fn install_reader(
        &mut self,
        session_uuid: String,
        event_tx: broadcast::Sender<PtyEvent>,
        kitty_enabled: Arc<AtomicBool>,
        cursor_visible: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        last_output_at: Arc<AtomicU64>,
        hub_event_tx: crate::hub::events::HubEventTx,
    ) -> Result<()> {
        let reader_stream = self
            .stream
            .try_clone()
            .context("dup session socket for reader")?;
        let (response_tx, response_rx) = std::sync::mpsc::channel::<Frame>();
        self.response_rx = Some(response_rx);
        self.reader_alive.store(true, Ordering::Release);
        let alive_flag = Arc::clone(&self.reader_alive);

        std::thread::Builder::new()
            .name(format!(
                "session-reader-{}",
                &session_uuid[..session_uuid.len().min(16)]
            ))
            .spawn(move || {
                session_reader(
                    reader_stream,
                    session_uuid,
                    event_tx,
                    kitty_enabled,
                    cursor_visible,
                    resize_pending,
                    last_output_at,
                    response_tx,
                    hub_event_tx,
                );
                alive_flag.store(false, Ordering::Release);
            })
            .context("spawn session reader thread")?;

        Ok(())
    }

    /// Whether the reader thread has been installed.
    pub fn has_reader(&self) -> bool {
        self.response_rx.is_some()
    }

    /// Whether the reader thread is still running.
    pub fn is_reader_alive(&self) -> bool {
        self.reader_alive.load(Ordering::Acquire)
    }

    /// Write raw PTY input bytes.
    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        let frame = encode_frame(FRAME_PTY_INPUT, data);
        self.stream.write_all(&frame).context("send PTY input")?;
        Ok(())
    }

    /// Send a resize command.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let frame = encode_json(
            FRAME_RESIZE,
            &serde_json::json!({"rows": rows, "cols": cols}),
        )?;
        self.stream.write_all(&frame).context("send resize")?;
        Ok(())
    }

    /// Request and receive an ANSI snapshot from the session process.
    ///
    /// The session process owns the terminal parser and generates snapshots
    /// on demand. This is the sole snapshot path for session-backed handles.
    ///
    /// The wire format is a dual-screen envelope:
    /// ```text
    /// [u32 LE: primary_len][primary VT bytes][alt VT bytes (optional)]
    /// ```
    /// When alt screen is active, both sections are present. The returned
    /// bytes are the combined VT output: primary + CSI ?1049h + alt.
    pub fn get_snapshot(&mut self) -> Result<Vec<u8>> {
        let req = encode_empty(FRAME_GET_SNAPSHOT);
        self.stream.write_all(&req).context("send GetSnapshot")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_SNAPSHOT)?;
        Ok(decode_dual_screen_snapshot(&frame.payload))
    }

    /// Request terminal mode flags from the session process.
    ///
    /// Used on reconnect to initialize the hub's state.
    pub fn get_mode_flags(&mut self) -> Result<ModeFlags> {
        let req = encode_empty(FRAME_GET_MODE_FLAGS);
        self.stream.write_all(&req).context("send GetModeFlags")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_MODE_FLAGS)?;
        frame.json()
    }

    /// Send a ping and wait for pong.
    pub fn ping(&mut self) -> Result<()> {
        let req = encode_empty(FRAME_PING);
        self.stream.write_all(&req).context("send ping")?;
        self.stream.flush()?;
        let _ = self.read_response(FRAME_PONG)?;
        Ok(())
    }

    /// Request clean shutdown.
    pub fn shutdown(&mut self) -> Result<()> {
        let req = encode_empty(FRAME_SHUTDOWN);
        self.stream.write_all(&req).context("send shutdown")?;
        Ok(())
    }

    /// Arm the tee log.
    pub fn arm_tee(&mut self, log_path: &str, cap_bytes: u64) -> Result<()> {
        let frame = encode_json(
            FRAME_ARM_TEE,
            &serde_json::json!({"log_path": log_path, "cap_bytes": cap_bytes}),
        )?;
        self.stream.write_all(&frame).context("send ArmTee")?;
        Ok(())
    }

    /// Read the next response frame of the expected type.
    ///
    /// Post-reader: receives from response channel (reader routes control frames here).
    /// Pre-reader: reads directly from socket, skipping async frames.
    fn read_response(&mut self, expected_type: u8) -> Result<Frame> {
        if let Some(ref rx) = self.response_rx {
            // Post-reader: responses arrive via channel from reader thread
            return rx
                .recv_timeout(RESPONSE_TIMEOUT)
                .context("timed out waiting for session control response via reader");
        }

        // Pre-reader: direct socket read (used during initial handshake/setup)
        let mut buf = [0u8; 8192];
        let deadline = std::time::Instant::now() + RESPONSE_TIMEOUT;

        loop {
            if std::time::Instant::now() >= deadline {
                bail!(
                    "timeout waiting for frame 0x{:02x} from session",
                    expected_type
                );
            }

            let n = self.stream.read(&mut buf).context("read from session")?;
            if n == 0 {
                bail!("session disconnected");
            }

            for frame in self.decoder.feed(&buf[..n]) {
                if frame.frame_type == FRAME_PTY_OUTPUT || frame.frame_type == FRAME_PROCESS_EXITED
                {
                    continue;
                }
                // Skip proactive event frames during pre-reader phase
                if (FRAME_TITLE_CHANGED..=FRAME_NOTIFICATION).contains(&frame.frame_type) {
                    continue;
                }
                if frame.frame_type == expected_type {
                    return Ok(frame);
                }
            }
        }
    }
}

// ─── Reader thread ───────────────────────────────────────────────────────────

/// Per-session reader thread.
///
/// Reads all frames from the session socket and routes them:
/// - PtyOutput → broadcast as PtyEvent::Output (no shadow screen parsing)
/// - Structured events (0x10-0x15) → map to PtyEvent variants, update atomics
/// - ProcessExited → send HubEvent
/// - Control responses (Snapshot, Screen, ModeFlags, Pong) → route to response_tx
fn session_reader(
    stream: UnixStream,
    session_uuid: String,
    event_tx: broadcast::Sender<PtyEvent>,
    kitty_enabled: Arc<AtomicBool>,
    cursor_visible: Arc<AtomicBool>,
    resize_pending: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    response_tx: std::sync::mpsc::Sender<Frame>,
    hub_event_tx: crate::hub::events::HubEventTx,
) {
    let mut decoder = FrameDecoder::new();
    let mut stream = stream;
    let _ = stream.set_read_timeout(None); // block indefinitely
    let mut buf = [0u8; 8192];

    log::info!(
        "[session-reader] started for {}",
        &session_uuid[..session_uuid.len().min(16)]
    );

    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                log::info!("[session-reader] session socket EOF");
                break;
            }
            Err(e) => {
                log::warn!("[session-reader] read error: {e}");
                break;
            }
            Ok(n) => n,
        };

        for frame in decoder.feed(&buf[..n]) {
            match frame.frame_type {
                FRAME_PTY_OUTPUT => {
                    let data = &frame.payload;

                    // Clear resize_pending — the app has redrawn after resize
                    resize_pending.store(false, Ordering::Release);

                    // Update idle timestamp
                    last_output_at.store(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        Ordering::Relaxed,
                    );

                    // Emit raw output observation for Lua hooks
                    let _ = hub_event_tx.send(crate::hub::events::HubEvent::PtyOutputObserved {
                        session_uuid: session_uuid.clone(),
                        data: data.to_vec(),
                    });

                    // Broadcast raw bytes to subscribers (TUI, browser, socket forwarders)
                    let _ = event_tx.send(PtyEvent::output(data.to_vec()));
                }

                // ── Structured events from session process (0x10-0x15) ──────
                FRAME_TITLE_CHANGED => {
                    let title = String::from_utf8_lossy(&frame.payload).into_owned();
                    let _ = event_tx.send(PtyEvent::title_changed(title));
                }

                FRAME_BELL => {
                    let _ = event_tx.send(PtyEvent::notification(AgentNotification::Bell));
                }

                FRAME_MODE_CHANGED => {
                    if let Ok(mode) = frame.json::<ModeChanged>() {
                        if let Some(kitty) = mode.kitty_enabled {
                            let old = kitty_enabled.load(Ordering::Relaxed);
                            if kitty != old {
                                kitty_enabled.store(kitty, Ordering::Relaxed);
                                let _ = event_tx.send(PtyEvent::kitty_changed(kitty));
                            }
                        }
                        if let Some(vis) = mode.cursor_visible {
                            cursor_visible.store(vis, Ordering::Relaxed);
                            let _ = event_tx.send(PtyEvent::cursor_visibility_changed(vis));
                        }
                        if let Some(focus) = mode.focus_reporting {
                            let _ = event_tx.send(PtyEvent::focus_reporting_changed(focus));
                        }
                        // Alt screen transitions are handled by the raw output
                        // stream — the TUI's parser processes CSI ?1049h/l directly.
                    }
                }

                FRAME_CWD_CHANGED => {
                    let cwd = String::from_utf8_lossy(&frame.payload).into_owned();
                    let _ = event_tx.send(PtyEvent::cwd_changed(cwd));
                }

                FRAME_PROMPT_MARK => {
                    if let Ok(payload) = frame.json::<PromptMarkPayload>() {
                        let mark = match payload.mark.as_str() {
                            "prompt_start" => Some(PromptMark::PromptStart),
                            "command_start" => Some(PromptMark::CommandStart),
                            "command_executed" => {
                                Some(PromptMark::CommandExecuted(payload.command))
                            }
                            "command_finished" => {
                                Some(PromptMark::CommandFinished(payload.exit_code))
                            }
                            _ => None,
                        };
                        if let Some(m) = mark {
                            let _ = event_tx.send(PtyEvent::prompt_mark(m));
                        }
                    }
                }

                FRAME_NOTIFICATION => {
                    if let Ok(payload) = frame.json::<NotificationPayload>() {
                        let notif = if payload.title.is_empty() {
                            AgentNotification::Osc9(if payload.body.is_empty() {
                                None
                            } else {
                                Some(payload.body)
                            })
                        } else {
                            AgentNotification::Osc777 {
                                title: payload.title,
                                body: payload.body,
                            }
                        };
                        let _ = event_tx.send(PtyEvent::notification(notif));
                    }
                }

                // ── Existing control frames ─────────────────────────────────
                FRAME_PROCESS_EXITED => {
                    let exit_code = frame
                        .json::<serde_json::Value>()
                        .ok()
                        .and_then(|v| v["exit_code"].as_i64())
                        .map(|c| c as i32);
                    let _ = hub_event_tx.send(crate::hub::events::HubEvent::SessionProcessExited {
                        session_uuid: session_uuid.clone(),
                        exit_code,
                    });
                    log::info!("[session-reader] process exited (code={:?})", exit_code);
                }

                _ => {
                    // Control response — route to RPC callers
                    if response_tx.send(frame).is_err() {
                        log::debug!("[session-reader] response channel closed");
                        return;
                    }
                }
            }
        }
    }

    // Session socket closed — notify hub
    let _ = hub_event_tx.send(crate::hub::events::HubEvent::SessionProcessExited {
        session_uuid,
        exit_code: None,
    });
}

/// Decode a dual-screen snapshot envelope into a single VT byte stream.
///
/// Wire format: `[u32 LE: primary_len][primary VT bytes][alt VT bytes]`
///
/// - Primary-only (no alt screen): returns primary bytes.
/// - Dual-screen (alt active): returns primary + CSI ?1049h + alt bytes,
///   which replays the normal screen then switches to alt and replays it.
///
/// Falls back to returning raw payload if the envelope is malformed
/// (e.g., old session process that doesn't use the envelope format).
fn decode_dual_screen_snapshot(payload: &[u8]) -> Vec<u8> {
    if payload.len() < 4 {
        return payload.to_vec();
    }

    let primary_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;

    if 4 + primary_len > payload.len() {
        // Malformed envelope — treat as raw snapshot for backwards compatibility
        return payload.to_vec();
    }

    let primary = &payload[4..4 + primary_len];
    let alt = &payload[4 + primary_len..];

    if alt.is_empty() {
        // Primary screen only
        primary.to_vec()
    } else {
        // Dual screen: primary + enter alt + alt content
        let mut out = Vec::with_capacity(primary.len() + 8 + alt.len());
        out.extend_from_slice(primary);
        out.extend_from_slice(b"\x1b[?1049h");
        out.extend_from_slice(alt);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_session_connection_type_compiles() {
        let _conn: SharedSessionConnection = Arc::new(Mutex::new(None));
    }

    #[test]
    fn decode_primary_only_snapshot() {
        let primary = b"hello world";
        let mut payload = (primary.len() as u32).to_le_bytes().to_vec();
        payload.extend_from_slice(primary);

        let result = decode_dual_screen_snapshot(&payload);
        assert_eq!(result, b"hello world");
    }

    #[test]
    fn decode_dual_screen_snapshot_combines() {
        let primary = b"normal screen";
        let alt = b"alt screen";
        let mut payload = (primary.len() as u32).to_le_bytes().to_vec();
        payload.extend_from_slice(primary);
        payload.extend_from_slice(alt);

        let result = decode_dual_screen_snapshot(&payload);
        assert!(result.starts_with(b"normal screen"));
        assert!(result.windows(8).any(|w| w == b"\x1b[?1049h"));
        assert!(result.ends_with(b"alt screen"));
    }

    #[test]
    fn decode_empty_payload() {
        let result = decode_dual_screen_snapshot(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn decode_malformed_falls_back_to_raw() {
        // primary_len says 1000 but payload is only 10 bytes
        let payload = [0xe8, 0x03, 0x00, 0x00, b'h', b'i'];
        let result = decode_dual_screen_snapshot(&payload);
        assert_eq!(result, payload);
    }
}
