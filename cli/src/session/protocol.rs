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
//! Same wire encoding as the broker protocol for familiarity and tooling reuse,
//! but dramatically fewer frame types since there's no session-id multiplexing.
//!
//! # Handshake
//!
//! After TCP-level connect, the Hub sends a `Hello` and the session responds
//! with `Welcome` containing session metadata. No capabilities negotiation —
//! protocol version is sufficient.

use std::collections::VecDeque;
use std::io::{self, Read, Write};

use anyhow::{bail, Context, Result};

// ─── Protocol version ────────────────────────────────────────────────────────

/// Current protocol version. Bump on breaking wire changes.
pub const PROTOCOL_VERSION: u8 = 1;

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

// ─── Handshake metadata ──────────────────────────────────────────────────────

/// Session metadata sent in the welcome handshake.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMetadata {
    pub session_uuid: String,
    pub pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub last_output_at: u64,
}

/// Terminal mode flags reported on reconnect.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModeFlags {
    pub kitty_enabled: bool,
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: u8,
    pub alt_screen: bool,
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

/// Encode a frame with JSON payload.
pub fn encode_json<T: serde::Serialize>(frame_type: u8, value: &T) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(value).context("serialize frame JSON")?;
    Ok(encode_frame(frame_type, &json))
}

// ─── Frame decoding ──────────────────────────────────────────────────────────

/// A decoded frame.
#[derive(Debug)]
pub struct Frame {
    pub frame_type: u8,
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
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
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
            let len = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]])
                as usize;
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
    stream.read_exact(&mut magic).context("read welcome magic")?;
    if &magic != WELCOME_MAGIC {
        bail!(
            "bad welcome magic: expected {:?}, got {:?}",
            WELCOME_MAGIC,
            magic
        );
    }
    let mut version = [0u8; 1];
    stream.read_exact(&mut version).context("read welcome version")?;

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
    stream.read_exact(&mut version).context("read hello version")?;

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

        let resize = Resize {
            rows: 24,
            cols: 80,
        };
        let encoded = encode_json(FRAME_RESIZE, &resize).unwrap();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded);
        assert_eq!(frames.len(), 1);
        let decoded: Resize = frames[0].json().unwrap();
        assert_eq!(decoded, resize);
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
        let mut session_out: Vec<u8> = Vec::new();

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
