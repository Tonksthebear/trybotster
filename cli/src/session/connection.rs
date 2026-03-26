//! Hub-side connection to a per-session process.
//!
//! Each `SessionConnection` owns a Unix socket stream to one session process.
//! After `install_reader()`, a dedicated thread reads all frames from the socket:
//!
//! - `PtyOutput` → fed into the shadow screen parser, broadcast as `PtyEvent::Output`
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

use alacritty_terminal::grid::Dimensions;

use anyhow::{bail, Context, Result};
use tokio::sync::broadcast;

use crate::agent::notification::detect_notifications;
use crate::agent::pty::{HubEventListener, PtyEvent};
use crate::agent::spawn::{scan_cwd, scan_prompt_marks};
use crate::terminal::AlacrittyParser;

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
    /// - `PtyOutput` → feeds shadow screen, broadcasts `PtyEvent::Output`
    /// - `ProcessExited` → `hub_event_tx` as `SessionProcessExited`
    /// - Control responses → `response_rx` for RPC callers
    ///
    /// After this call, `read_response()` uses the channel instead of
    /// reading the socket directly.
    pub(crate) fn install_reader(
        &mut self,
        session_uuid: String,
        shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,
        shadow_listener: HubEventListener,
        event_tx: broadcast::Sender<PtyEvent>,
        kitty_enabled: Arc<AtomicBool>,
        cursor_visible: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        last_output_at: Arc<AtomicU64>,
        detect_notifs: bool,
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
                    shadow_screen,
                    shadow_listener,
                    event_tx,
                    kitty_enabled,
                    cursor_visible,
                    resize_pending,
                    last_output_at,
                    detect_notifs,
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
    /// Used on reconnect to populate the hub's shadow screen.
    /// During normal operation, the hub generates snapshots locally.
    pub fn get_snapshot(&mut self) -> Result<Vec<u8>> {
        let req = encode_empty(FRAME_GET_SNAPSHOT);
        self.stream.write_all(&req).context("send GetSnapshot")?;
        self.stream.flush()?;
        let frame = self.read_response(FRAME_SNAPSHOT)?;
        Ok(frame.payload)
    }

    /// Request terminal mode flags from the session process.
    ///
    /// Used on reconnect to initialize the hub's shadow screen state.
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
/// - PtyOutput → feed shadow screen, update state atomics, broadcast PtyEvent::Output
/// - ProcessExited → send HubEvent
/// - Control responses → route to response_tx for RPC callers
fn session_reader(
    stream: UnixStream,
    session_uuid: String,
    shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,
    shadow_listener: HubEventListener,
    event_tx: broadcast::Sender<PtyEvent>,
    kitty_enabled: Arc<AtomicBool>,
    cursor_visible: Arc<AtomicBool>,
    resize_pending: Arc<AtomicBool>,
    last_output_at: Arc<AtomicU64>,
    detect_notifs: bool,
    response_tx: std::sync::mpsc::Sender<Frame>,
    hub_event_tx: crate::hub::events::HubEventTx,
) {
    let mut decoder = FrameDecoder::new();
    let mut stream = stream;
    let _ = stream.set_read_timeout(None); // block indefinitely
    let mut buf = [0u8; 8192];
    let mut last_cursor_visible: Option<bool> = None;
    let mut last_focus_reporting: Option<bool> = None;
    let mut last_alt_screen: Option<bool> = None;

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
                    let probe_descriptions =
                        crate::hub::terminal_profile::describe_probe_sequences(data);
                    if !probe_descriptions.is_empty() {
                        log::info!(
                            "[session-reader][PTY-PROBE] session={} observed {}",
                            session_uuid,
                            probe_descriptions.join(", ")
                        );
                    }

                    // Feed shadow screen — hub becomes terminal state authority
                    let (new_kitty, new_visible, new_focus_reporting, new_alt_screen) =
                        if let Ok(mut p) = shadow_screen.lock() {
                            p.process(data);
                            (
                                p.kitty_enabled(),
                                !p.cursor_hidden(),
                                p.focus_reporting(),
                                p.alt_screen_active(),
                            )
                        } else {
                            (false, true, false, false)
                        };

                    // Drain color responses that the shadow screen's alacritty
                    // produced from cached RGB values during process().
                    for resp in shadow_listener.drain_color_responses() {
                        let _ = hub_event_tx.send(
                            crate::hub::events::HubEvent::ColorResponse {
                                session_uuid: session_uuid.clone(),
                                response: resp.response,
                            },
                        );
                    }

                    // Update state atomics
                    let old_kitty = kitty_enabled.load(Ordering::Relaxed);
                    if new_kitty != old_kitty {
                        kitty_enabled.store(new_kitty, Ordering::Relaxed);
                        let _ = event_tx.send(PtyEvent::kitty_changed(new_kitty));
                    }
                    if last_cursor_visible != Some(new_visible) {
                        last_cursor_visible = Some(new_visible);
                        cursor_visible.store(new_visible, Ordering::Relaxed);
                        let _ = event_tx.send(PtyEvent::cursor_visibility_changed(new_visible));
                    }
                    if last_focus_reporting != Some(new_focus_reporting) {
                        last_focus_reporting = Some(new_focus_reporting);
                        let _ = event_tx.send(PtyEvent::focus_reporting_changed(new_focus_reporting));
                    }
                    resize_pending.store(false, Ordering::Release);

                    // Alt screen transition: prepare a scrollback refresh.
                    // Sent AFTER PtyEvent::Output so the old parser handles the
                    // raw bytes first (including CSI ?1049l), then gets replaced.
                    let pending_alt_scrollback = if last_alt_screen != Some(new_alt_screen) {
                        let was_alt = last_alt_screen.unwrap_or(false);
                        last_alt_screen = Some(new_alt_screen);
                        // Only on exit (alt → normal). On entry the alt screen
                        // starts empty and the app redraws immediately.
                        if was_alt && !new_alt_screen {
                            if let Ok(p) = shadow_screen.lock() {
                                let rows = p.term().grid().screen_lines() as u16;
                                let cols = p.term().grid().columns() as u16;
                                let snapshot =
                                    crate::terminal::generate_ansi_snapshot(&*p, false);
                                Some((snapshot, rows, cols))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Update idle timestamp
                    last_output_at.store(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        Ordering::Relaxed,
                    );

                    // OSC scanning (CWD, notifications, prompt marks)
                    if detect_notifs {
                        for notif in detect_notifications(data) {
                            let _ = event_tx.send(PtyEvent::notification(notif));
                        }
                    }
                    if let Some(cwd) = scan_cwd(data) {
                        let _ = event_tx.send(PtyEvent::cwd_changed(cwd));
                    }
                    for mark in scan_prompt_marks(data) {
                        let _ = event_tx.send(PtyEvent::prompt_mark(mark));
                    }

                    // Session-backed PTYs must emit raw output observations from
                    // the reader itself so startup probe queries are not missed
                    // before the Lua notification watcher subscribes.
                    let _ = hub_event_tx.send(crate::hub::events::HubEvent::PtyOutputObserved {
                        session_uuid: session_uuid.clone(),
                        data: data.to_vec(),
                    });

                    // Broadcast raw bytes to subscribers (forwarders)
                    let _ = event_tx.send(PtyEvent::output(data.to_vec()));

                    // Send alt screen scrollback refresh AFTER Output so clients
                    // process the raw bytes (on the old parser) before replacing it.
                    if let Some((snap_data, snap_rows, snap_cols)) = pending_alt_scrollback {
                        log::info!(
                            "[session-reader] alt screen exited, sending {} byte scrollback refresh",
                            snap_data.len()
                        );
                        let _ = event_tx.send(PtyEvent::AltScreenScrollback {
                            data: snap_data,
                            rows: snap_rows,
                            cols: snap_cols,
                        });
                    }
                }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_session_connection_type_compiles() {
        let _conn: SharedSessionConnection = Arc::new(Mutex::new(None));
    }
}
