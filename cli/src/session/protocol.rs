//! Per-session process wire protocol.
//!
//! Each session process communicates with the Hub over a dedicated Unix socket.
//! No multiplexing — one socket, one session, one protocol instance.
//!
//! # Frame format
//!
//! ```text
//! [u32 LE: payload_len + 1][u8: frame_type][payload_bytes]
//! ```
//!
//! Uses the same wire encoding as the earlier socket protocol for familiarity
//! and tooling reuse, but dramatically fewer frame types since there's no
//! session-id multiplexing.
//!
//! # Handshake
//!
//! After TCP-level connect, the Hub sends a `Hello` and the session responds
//! with `Welcome` containing session metadata. No capabilities negotiation —
//! protocol version is sufficient.

use std::io::{Read, Write};

use anyhow::{bail, Context, Result};

// ─── Protocol version ────────────────────────────────────────────────────────

/// Current protocol version. Bump on breaking wire changes.
pub const PROTOCOL_VERSION: u8 = 2;

/// Magic bytes for hub → session hello.
pub const HELLO_MAGIC: &[u8; 4] = b"SPH1";

/// Magic bytes for session → hub welcome.
pub const WELCOME_MAGIC: &[u8; 4] = b"SPA1";

// ─── Frame types ─────────────────────────────────────────────────────────────

/// Hub → Session: raw PTY input bytes.
pub const FRAME_PTY_INPUT: u8 = 0x01;

/// Session → Hub: raw PTY output bytes.
pub const FRAME_PTY_OUTPUT: u8 = 0x02;

/// Hub → Session: resize command (JSON payload: `{"rows": u16, "cols": u16}`).
pub const FRAME_RESIZE: u8 = 0x03;

/// Hub → Session: arm tee log (JSON payload: `{"log_path": str, "cap_bytes": u64}`).
pub const FRAME_ARM_TEE: u8 = 0x04;

/// Hub → Session: request ANSI snapshot of current terminal state.
pub const FRAME_GET_SNAPSHOT: u8 = 0x05;

/// Session → Hub: ANSI snapshot response.
pub const FRAME_SNAPSHOT: u8 = 0x06;

/// Session → Hub: child process exited (JSON payload: `{"exit_code": i32|null}`).
pub const FRAME_PROCESS_EXITED: u8 = 0x07;

/// Hub → Session: keepalive ping.
pub const FRAME_PING: u8 = 0x08;

/// Session → Hub: keepalive pong.
pub const FRAME_PONG: u8 = 0x09;

/// Hub → Session: request clean shutdown (kill child, exit).
pub const FRAME_SHUTDOWN: u8 = 0x0A;

/// Hub → Session: set reconnect timeout (JSON payload: `{"seconds": u64}`).
pub const FRAME_SET_TIMEOUT: u8 = 0x0B;

/// Hub → Session: request terminal mode flags.
pub const FRAME_GET_MODE_FLAGS: u8 = 0x0C;

/// Session → Hub: terminal mode flags response (JSON payload).
pub const FRAME_MODE_FLAGS: u8 = 0x0D;

/// Hub → Session: request plain text screen contents.
pub const FRAME_GET_SCREEN: u8 = 0x0E;

/// Session → Hub: plain text screen response.
pub const FRAME_SCREEN: u8 = 0x0F;

// ─── Proactive state change frames (session → hub) ─────────────────────

/// Session → Hub: window title changed (string payload: new title).
pub const FRAME_TITLE_CHANGED: u8 = 0x10;

/// Session → Hub: bell character received (empty payload).
pub const FRAME_BELL: u8 = 0x11;

/// Session → Hub: terminal mode changed (JSON payload: only changed fields).
pub const FRAME_MODE_CHANGED: u8 = 0x12;

/// Session → Hub: working directory changed (string payload: new CWD path).
pub const FRAME_CWD_CHANGED: u8 = 0x13;

/// Session → Hub: shell prompt mark detected (JSON payload: `{"mark": str}`).
pub const FRAME_PROMPT_MARK: u8 = 0x14;

/// Session → Hub: OSC notification detected (JSON payload: `{"title": str, "body": str}`).
pub const FRAME_NOTIFICATION: u8 = 0x15;

// ─── Handshake metadata ──────────────────────────────────────────────────────

/// Session metadata sent in the welcome handshake.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMetadata {
    /// Unique session identifier.
    pub session_uuid: String,
    /// PID of the session process.
    pub pid: u32,
    /// Current PTY row count.
    pub rows: u16,
    /// Current PTY column count.
    pub cols: u16,
    /// Unix timestamp of last PTY output.
    pub last_output_at: u64,
}

/// Terminal mode flags reported on reconnect.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModeFlags {
    /// Kitty keyboard protocol enabled.
    pub kitty_enabled: bool,
    /// Cursor is visible.
    pub cursor_visible: bool,
    /// Bracketed paste mode enabled.
    pub bracketed_paste: bool,
    /// Mouse tracking mode (0=off, 1000/1002/1003/1006).
    pub mouse_mode: u8,
    /// Alternate screen buffer active.
    pub alt_screen: bool,
    /// Focus reporting mode enabled (DECSET 1004).
    #[serde(default)]
    pub focus_reporting: bool,
    /// Application cursor keys mode (DECCKM, mode 1).
    #[serde(default)]
    pub application_cursor: bool,
}

/// Incremental mode change pushed proactively by the session.
///
/// Only changed fields are present (None = unchanged). This avoids the hub
/// needing to re-parse PTY output to detect mode transitions.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModeChanged {
    /// Kitty keyboard protocol toggled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kitty_enabled: Option<bool>,
    /// Cursor visibility changed (DECTCEM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_visible: Option<bool>,
    /// Alternate screen buffer toggled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_screen: Option<bool>,
    /// Mouse tracking mode changed (0=off, 1000/1002/1003/1006).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_mode: Option<u8>,
    /// Bracketed paste mode toggled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bracketed_paste: Option<bool>,
    /// Focus reporting mode toggled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_reporting: Option<bool>,
    /// Application cursor keys mode toggled (DECCKM).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_cursor: Option<bool>,
}

/// OSC notification payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotificationPayload {
    /// Notification title (empty for OSC 9).
    pub title: String,
    /// Notification body text.
    pub body: String,
}

/// Prompt mark payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptMarkPayload {
    /// One of: "prompt_start", "command_start", "command_executed", "command_finished".
    pub mark: String,
    /// Optional command text (for command_executed marks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Optional exit code (for command_finished marks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

// ─── Frame encoding ──────────────────────────────────────────────────────────

/// Encode a frame: `[u32 LE: payload_len + 1][u8: frame_type][payload]`.
pub fn encode_frame(frame_type: u8, payload: &[u8]) -> Vec<u8> {
    let total = 1 + payload.len();
    let mut buf = Vec::with_capacity(4 + total);
    buf.extend_from_slice(&(total as u32).to_le_bytes());
    buf.push(frame_type);
    buf.extend_from_slice(payload);
    buf
}

/// Encode a frame with no payload.
pub fn encode_empty(frame_type: u8) -> Vec<u8> {
    encode_frame(frame_type, &[])
}

/// Encode a frame with a UTF-8 string payload.
pub fn encode_string(frame_type: u8, s: &str) -> Vec<u8> {
    encode_frame(frame_type, s.as_bytes())
}

/// Encode a frame with JSON payload.
pub fn encode_json<T: serde::Serialize>(frame_type: u8, value: &T) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(value).context("serialize frame JSON")?;
    Ok(encode_frame(frame_type, &json))
}

// ─── Frame decoding ──────────────────────────────────────────────────────────

/// A decoded frame.
#[derive(Debug)]
pub struct Frame {
    /// Wire frame type byte.
    pub frame_type: u8,
    /// Raw frame payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// Parse payload as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.payload)
            .with_context(|| format!("parse frame 0x{:02x} JSON", self.frame_type))
    }
}

/// Incremental frame decoder.
///
/// Feed bytes via `feed()`, drain complete frames from the returned `Vec`.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Create a new frame decoder with default buffer capacity.
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8192),
        }
    }

    /// Feed raw bytes, return any complete frames.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();

        loop {
            if self.buf.len() < 4 {
                break;
            }
            let len =
                u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
            if len == 0 || self.buf.len() < 4 + len {
                break;
            }
            let frame_type = self.buf[4];
            let payload = self.buf[5..4 + len].to_vec();
            // Remove consumed bytes
            self.buf.drain(..4 + len);
            frames.push(Frame {
                frame_type,
                payload,
            });
        }

        frames
    }
}

// ─── Handshake ───────────────────────────────────────────────────────────────

/// Perform the hub side of the handshake: send hello, receive welcome + metadata.
pub fn handshake_hub(stream: &mut (impl Read + Write)) -> Result<(u8, SessionMetadata)> {
    // Send hello
    stream.write_all(HELLO_MAGIC)?;
    stream.write_all(&[PROTOCOL_VERSION])?;
    stream.flush()?;

    // Read welcome
    let mut magic = [0u8; 4];
    stream
        .read_exact(&mut magic)
        .context("read welcome magic")?;
    if &magic != WELCOME_MAGIC {
        bail!(
            "bad welcome magic: expected {:?}, got {:?}",
            WELCOME_MAGIC,
            magic
        );
    }
    let mut version = [0u8; 1];
    stream
        .read_exact(&mut version)
        .context("read welcome version")?;

    // Read metadata length (u32 LE) + JSON
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .context("read metadata length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 64 * 1024 {
        bail!("metadata too large: {len} bytes");
    }
    let mut json_buf = vec![0u8; len];
    stream.read_exact(&mut json_buf).context("read metadata")?;
    let metadata: SessionMetadata =
        serde_json::from_slice(&json_buf).context("parse session metadata")?;

    Ok((version[0], metadata))
}

/// Perform the session side of the handshake: receive hello, send welcome + metadata.
pub fn handshake_session(
    stream: &mut (impl Read + Write),
    metadata: &SessionMetadata,
) -> Result<u8> {
    // Read hello
    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic).context("read hello magic")?;
    if &magic != HELLO_MAGIC {
        bail!(
            "bad hello magic: expected {:?}, got {:?}",
            HELLO_MAGIC,
            magic
        );
    }
    let mut version = [0u8; 1];
    stream
        .read_exact(&mut version)
        .context("read hello version")?;

    // Send welcome
    stream.write_all(WELCOME_MAGIC)?;
    stream.write_all(&[PROTOCOL_VERSION])?;

    // Send metadata as length-prefixed JSON
    let json = serde_json::to_vec(metadata).context("serialize metadata")?;
    stream.write_all(&(json.len() as u32).to_le_bytes())?;
    stream.write_all(&json)?;
    stream.flush()?;

    Ok(version[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn encode_decode_roundtrip() {
        let data = b"hello world";
        let encoded = encode_frame(FRAME_PTY_OUTPUT, data);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_PTY_OUTPUT);
        assert_eq!(frames[0].payload, data);
    }

    #[test]
    fn encode_decode_empty_frame() {
        let encoded = encode_empty(FRAME_PING);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_PING);
        assert!(frames[0].payload.is_empty());
    }

    #[test]
    fn decode_partial_then_complete() {
        let encoded = encode_frame(FRAME_PTY_INPUT, b"test");
        let mut decoder = FrameDecoder::new();

        // Feed first 3 bytes (incomplete header)
        let frames = decoder.feed(&encoded[..3]);
        assert!(frames.is_empty());

        // Feed rest
        let frames = decoder.feed(&encoded[3..]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"test");
    }

    #[test]
    fn decode_multiple_frames() {
        let mut data = Vec::new();
        data.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"one"));
        data.extend_from_slice(&encode_frame(FRAME_PTY_OUTPUT, b"two"));
        data.extend_from_slice(&encode_empty(FRAME_PONG));

        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&data);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].payload, b"one");
        assert_eq!(frames[1].payload, b"two");
        assert!(frames[2].payload.is_empty());
    }

    #[test]
    fn json_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Resize {
            rows: u16,
            cols: u16,
        }

        let resize = Resize { rows: 24, cols: 80 };
        let encoded = encode_json(FRAME_RESIZE, &resize).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        let decoded: Resize = frames[0].json().unwrap();
        assert_eq!(decoded, resize);
    }

    #[test]
    fn mode_changed_sparse_json() {
        let mode = ModeChanged {
            kitty_enabled: Some(true),
            alt_screen: Some(false),
            ..Default::default()
        };
        let encoded = encode_json(FRAME_MODE_CHANGED, &mode).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_MODE_CHANGED);
        let decoded: ModeChanged = frames[0].json().unwrap();
        assert_eq!(decoded.kitty_enabled, Some(true));
        assert_eq!(decoded.alt_screen, Some(false));
        assert!(decoded.cursor_visible.is_none());
        assert!(decoded.mouse_mode.is_none());
        assert!(decoded.bracketed_paste.is_none());
        assert!(decoded.focus_reporting.is_none());
        assert!(decoded.application_cursor.is_none());
    }

    #[test]
    fn string_frame_roundtrip() {
        let encoded = encode_string(FRAME_TITLE_CHANGED, "My Terminal");
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_TITLE_CHANGED);
        assert_eq!(
            std::str::from_utf8(&frames[0].payload).unwrap(),
            "My Terminal"
        );
    }

    #[test]
    fn notification_payload_roundtrip() {
        let notif = NotificationPayload {
            title: "Build".to_string(),
            body: "Done".to_string(),
        };
        let encoded = encode_json(FRAME_NOTIFICATION, &notif).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        let decoded: NotificationPayload = frames[0].json().unwrap();
        assert_eq!(decoded.title, "Build");
        assert_eq!(decoded.body, "Done");
    }

    #[test]
    fn prompt_mark_payload_roundtrip() {
        let mark = PromptMarkPayload {
            mark: "command_finished".to_string(),
            command: None,
            exit_code: Some(0),
        };
        let encoded = encode_json(FRAME_PROMPT_MARK, &mark).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        let decoded: PromptMarkPayload = frames[0].json().unwrap();
        assert_eq!(decoded.mark, "command_finished");
        assert!(decoded.command.is_none());
        assert_eq!(decoded.exit_code, Some(0));
    }

    #[test]
    fn bell_frame() {
        let encoded = encode_empty(FRAME_BELL);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame_type, FRAME_BELL);
        assert!(frames[0].payload.is_empty());
    }

    #[test]
    fn cwd_changed_frame() {
        let encoded = encode_string(FRAME_CWD_CHANGED, "/home/user/project");
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames[0].frame_type, FRAME_CWD_CHANGED);
        assert_eq!(
            std::str::from_utf8(&frames[0].payload).unwrap(),
            "/home/user/project"
        );
    }

    #[test]
    fn handshake_roundtrip() {
        let metadata = SessionMetadata {
            session_uuid: "sess-test-123".to_string(),
            pid: 42,
            rows: 24,
            cols: 80,
            last_output_at: 0,
        };

        // Simulate hub → session → hub via in-memory buffers
        let mut hub_to_session = Vec::new();
        let mut session_to_hub = Vec::new();

        // Hub writes hello
        hub_to_session.extend_from_slice(HELLO_MAGIC);
        hub_to_session.push(PROTOCOL_VERSION);

        // Session reads hello, writes welcome
        let mut cursor = Cursor::new(&hub_to_session);
        let _session_out: Vec<u8> = Vec::new();

        // Manual session-side handshake (read from cursor, write to session_out)
        let mut magic = [0u8; 4];
        cursor.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, HELLO_MAGIC);
        let mut ver = [0u8; 1];
        cursor.read_exact(&mut ver).unwrap();

        session_to_hub.extend_from_slice(WELCOME_MAGIC);
        session_to_hub.push(PROTOCOL_VERSION);
        let json = serde_json::to_vec(&metadata).unwrap();
        session_to_hub.extend_from_slice(&(json.len() as u32).to_le_bytes());
        session_to_hub.extend_from_slice(&json);

        // Hub reads welcome
        let mut cursor = Cursor::new(&session_to_hub);
        let mut magic = [0u8; 4];
        cursor.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, WELCOME_MAGIC);
        let mut ver = [0u8; 1];
        cursor.read_exact(&mut ver).unwrap();
        let mut len_buf = [0u8; 4];
        cursor.read_exact(&mut len_buf).unwrap();
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut json_buf = vec![0u8; len];
        cursor.read_exact(&mut json_buf).unwrap();
        let decoded: SessionMetadata = serde_json::from_slice(&json_buf).unwrap();

        assert_eq!(decoded.session_uuid, "sess-test-123");
        assert_eq!(decoded.pid, 42);
        assert_eq!(decoded.rows, 24);
        assert_eq!(decoded.cols, 80);
    }
}
