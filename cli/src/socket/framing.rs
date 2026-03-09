//! Wire protocol codec for Unix domain socket IPC.
//!
//! Length-prefixed frames with a type byte:
//!
//! ```text
//! [u32 LE length] [u8 type] [payload: length-1 bytes]
//! ```
//!
//! Frame types:
//! - `0x01`: JSON message (UTF-8 `serde_json::Value`)
//! - `0x02`: PTY output binary (hub→client) — `[u16 uuid_len][uuid][raw bytes]`
//! - `0x03`: PTY input binary (client→hub) — `[u16 uuid_len][uuid][raw bytes]`
//! - `0x04`: PTY scrollback (hub→client) — `[u16 uuid_len][uuid][u16 rows][u16 cols][u8 kitty][raw bytes]`
//!   (legacy v1 decode fallback: `[u16 uuid_len][uuid][u8 kitty][raw bytes]`)
//! - `0x05`: Process exited (hub→client) — `[u16 uuid_len][uuid][i32 exit_code]`
//! - `0x06`: Raw binary (bidirectional) — `[raw bytes]`

use anyhow::{anyhow, bail, Result};

/// Maximum frame payload size (16 MB).
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Frame type constants.
pub mod frame_type {
    /// JSON control message.
    pub const JSON: u8 = 0x01;
    /// PTY output binary (hub → client).
    pub const PTY_OUTPUT: u8 = 0x02;
    /// PTY input binary (client → hub).
    pub const PTY_INPUT: u8 = 0x03;
    /// PTY scrollback (hub → client).
    pub const SCROLLBACK: u8 = 0x04;
    /// Process exited (hub → client).
    pub const PROCESS_EXITED: u8 = 0x05;
    /// Raw binary data (bidirectional, no PTY routing header).
    pub const BINARY: u8 = 0x06;
}

/// A decoded frame from the wire protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// JSON control message.
    Json(serde_json::Value),

    /// PTY output data (hub → client).
    PtyOutput {
        /// Session UUID identifying the PTY.
        session_uuid: String,
        /// Raw PTY output bytes.
        data: Vec<u8>,
    },

    /// PTY input data (client → hub).
    PtyInput {
        /// Session UUID identifying the target PTY.
        session_uuid: String,
        /// Raw keyboard input bytes.
        data: Vec<u8>,
    },

    /// PTY scrollback data (hub → client, sent once on subscribe).
    Scrollback {
        /// Session UUID identifying the PTY.
        session_uuid: String,
        /// Authoritative rows used to generate this snapshot.
        rows: u16,
        /// Authoritative cols used to generate this snapshot.
        cols: u16,
        /// Whether kitty keyboard protocol is active.
        kitty_enabled: bool,
        /// Raw scrollback bytes.
        data: Vec<u8>,
    },

    /// PTY process exited (hub → client).
    ProcessExited {
        /// Session UUID identifying the PTY.
        session_uuid: String,
        /// Exit code (None if killed by signal).
        exit_code: Option<i32>,
    },

    /// Raw binary data (no PTY routing header).
    ///
    /// Used by `socket.send_binary()` for plugin-level binary messaging.
    /// Not interpreted as PTY data.
    Binary(Vec<u8>),
}

/// Encode a session UUID as a length-prefixed byte sequence.
fn encode_session_uuid(payload: &mut Vec<u8>, session_uuid: &str) {
    let uuid_bytes = session_uuid.as_bytes();
    payload.extend_from_slice(&(uuid_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(uuid_bytes);
}

/// Decode a length-prefixed session UUID from a payload at the start.
///
/// Returns `(session_uuid, bytes_consumed)`.
fn decode_session_uuid(payload: &[u8]) -> Result<(String, usize)> {
    if payload.len() < 2 {
        bail!("Frame too short for session UUID length prefix");
    }
    let uuid_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let total = 2 + uuid_len;
    if payload.len() < total {
        bail!(
            "Frame too short for session UUID: need {total}, have {}",
            payload.len()
        );
    }
    let uuid = std::str::from_utf8(&payload[2..total])
        .map_err(|e| anyhow!("Invalid UTF-8 in session UUID: {e}"))?
        .to_string();
    Ok((uuid, total))
}

impl Frame {
    /// Encode this frame into a wire-format byte vector.
    ///
    /// Returns `[u32 LE length][u8 type][payload]`.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Json(value) => {
                let payload = serde_json::to_vec(value).expect("JSON serialization cannot fail");
                encode_raw(frame_type::JSON, &payload)
            }
            Frame::PtyOutput { session_uuid, data } => {
                let mut payload = Vec::with_capacity(2 + session_uuid.len() + data.len());
                encode_session_uuid(&mut payload, session_uuid);
                payload.extend_from_slice(data);
                encode_raw(frame_type::PTY_OUTPUT, &payload)
            }
            Frame::PtyInput { session_uuid, data } => {
                let mut payload = Vec::with_capacity(2 + session_uuid.len() + data.len());
                encode_session_uuid(&mut payload, session_uuid);
                payload.extend_from_slice(data);
                encode_raw(frame_type::PTY_INPUT, &payload)
            }
            Frame::Scrollback {
                session_uuid,
                rows,
                cols,
                kitty_enabled,
                data,
            } => {
                let mut payload =
                    Vec::with_capacity(2 + session_uuid.len() + 2 + 2 + 1 + data.len());
                encode_session_uuid(&mut payload, session_uuid);
                payload.extend_from_slice(&rows.to_le_bytes());
                payload.extend_from_slice(&cols.to_le_bytes());
                payload.push(u8::from(*kitty_enabled));
                payload.extend_from_slice(data);
                encode_raw(frame_type::SCROLLBACK, &payload)
            }
            Frame::ProcessExited {
                session_uuid,
                exit_code,
            } => {
                let mut payload = Vec::with_capacity(2 + session_uuid.len() + 4);
                encode_session_uuid(&mut payload, session_uuid);
                payload.extend_from_slice(&exit_code.unwrap_or(-1).to_le_bytes());
                encode_raw(frame_type::PROCESS_EXITED, &payload)
            }
            Frame::Binary(data) => encode_raw(frame_type::BINARY, data),
        }
    }
}

/// Encode a raw frame with type byte and payload.
fn encode_raw(frame_type: u8, payload: &[u8]) -> Vec<u8> {
    let length = (payload.len() + 1) as u32; // +1 for type byte
    let mut buf = Vec::with_capacity(4 + 1 + payload.len());
    buf.extend_from_slice(&length.to_le_bytes());
    buf.push(frame_type);
    buf.extend_from_slice(payload);
    buf
}

/// Decode a single frame from a type byte and payload.
fn decode_frame(frame_type: u8, payload: &[u8]) -> Result<Frame> {
    match frame_type {
        frame_type::JSON => {
            let value: serde_json::Value =
                serde_json::from_slice(payload).map_err(|e| anyhow!("Invalid JSON frame: {e}"))?;
            Ok(Frame::Json(value))
        }
        frame_type::PTY_OUTPUT => {
            let (session_uuid, consumed) = decode_session_uuid(payload)?;
            Ok(Frame::PtyOutput {
                session_uuid,
                data: payload[consumed..].to_vec(),
            })
        }
        frame_type::PTY_INPUT => {
            let (session_uuid, consumed) = decode_session_uuid(payload)?;
            Ok(Frame::PtyInput {
                session_uuid,
                data: payload[consumed..].to_vec(),
            })
        }
        frame_type::SCROLLBACK => {
            let (session_uuid, consumed) = decode_session_uuid(payload)?;
            // Backward-compatible decode:
            // v2 = [rows:u16][cols:u16][kitty:u8][data...]
            // v1 = [kitty:u8][data...]
            if payload.len() >= consumed + 5 {
                let rows = u16::from_le_bytes([payload[consumed], payload[consumed + 1]]);
                let cols = u16::from_le_bytes([payload[consumed + 2], payload[consumed + 3]]);
                let kitty_byte = payload[consumed + 4];
                let plausible_dims = rows >= 2 && cols >= 2 && rows <= 1024 && cols <= 1024;
                let plausible_kitty = kitty_byte <= 1;
                if plausible_dims && plausible_kitty {
                    let kitty_enabled = kitty_byte != 0;
                    return Ok(Frame::Scrollback {
                        session_uuid,
                        rows,
                        cols,
                        kitty_enabled,
                        data: payload[consumed + 5..].to_vec(),
                    });
                }
            }
            if payload.len() >= consumed + 1 {
                let kitty_enabled = payload[consumed] != 0;
                return Ok(Frame::Scrollback {
                    session_uuid,
                    // Legacy payloads carry no source dimensions.
                    // Callers should treat 0x0 as "use local panel dims".
                    rows: 0,
                    cols: 0,
                    kitty_enabled,
                    data: payload[consumed + 1..].to_vec(),
                });
            }
            bail!("Scrollback frame too short for kitty or rows/cols/kitty")
        }
        frame_type::PROCESS_EXITED => {
            let (session_uuid, consumed) = decode_session_uuid(payload)?;
            if payload.len() < consumed + 4 {
                bail!("Process exited frame too short for exit code");
            }
            let raw_code = i32::from_le_bytes([
                payload[consumed],
                payload[consumed + 1],
                payload[consumed + 2],
                payload[consumed + 3],
            ]);
            let exit_code = if raw_code == -1 { None } else { Some(raw_code) };
            Ok(Frame::ProcessExited {
                session_uuid,
                exit_code,
            })
        }
        frame_type::BINARY => Ok(Frame::Binary(payload.to_vec())),
        _ => bail!("Unknown frame type: 0x{frame_type:02x}"),
    }
}

/// Incremental frame decoder that handles partial reads.
///
/// Feed bytes via [`FrameDecoder::feed`] and extract complete frames.
/// Handles TCP-style byte stream reassembly.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Create a new decoder with empty buffer.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Feed bytes into the decoder and extract all complete frames.
    ///
    /// Returns decoded frames. Incomplete data is buffered for the next call.
    ///
    /// # Errors
    ///
    /// Returns an error if a frame is malformed or exceeds the size limit.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();

        loop {
            // Need at least 4 bytes for the length header
            if self.buf.len() < 4 {
                break;
            }

            let length = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);

            if length == 0 {
                bail!("Invalid frame: zero length");
            }
            if length > MAX_FRAME_SIZE {
                bail!("Frame too large: {length} bytes (max {MAX_FRAME_SIZE})");
            }

            let total = 4 + length as usize;
            if self.buf.len() < total {
                break; // Incomplete frame, wait for more data
            }

            // Extract the complete frame
            let frame_type = self.buf[4];
            let payload = &self.buf[5..total];
            let frame = decode_frame(frame_type, payload)?;
            frames.push(frame);

            // Remove consumed bytes
            self.buf.drain(..total);
        }

        Ok(frames)
    }

    /// Returns true if the decoder has buffered partial data.
    pub fn has_partial(&self) -> bool {
        !self.buf.is_empty()
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_round_trip() {
        let frame = Frame::Json(serde_json::json!({"type": "subscribe", "channel": "hub"}));
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
        assert!(!decoder.has_partial());
    }

    #[test]
    fn test_pty_output_round_trip() {
        let frame = Frame::PtyOutput {
            session_uuid: "sess-abc-123".to_string(),
            data: b"hello world".to_vec(),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_pty_input_round_trip() {
        let frame = Frame::PtyInput {
            session_uuid: "sess-def-456".to_string(),
            data: vec![0x1b, b'[', b'A'], // Up arrow
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_scrollback_round_trip() {
        let frame = Frame::Scrollback {
            session_uuid: "sess-ghi-789".to_string(),
            rows: 24,
            cols: 80,
            kitty_enabled: true,
            data: b"scrollback content".to_vec(),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_scrollback_legacy_v1_decode() {
        let session_uuid = "sess-legacy";
        let data = b"legacy scrollback".to_vec();

        let mut payload = Vec::new();
        payload.extend_from_slice(&(session_uuid.len() as u16).to_le_bytes());
        payload.extend_from_slice(session_uuid.as_bytes());
        payload.push(1u8); // kitty enabled
        payload.extend_from_slice(&data);

        let encoded = encode_raw(frame_type::SCROLLBACK, &payload);
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            Frame::Scrollback {
                session_uuid: decoded_uuid,
                rows,
                cols,
                kitty_enabled,
                data: decoded_data,
            } => {
                assert_eq!(decoded_uuid, session_uuid);
                assert_eq!((*rows, *cols), (0, 0));
                assert!(*kitty_enabled);
                assert_eq!(decoded_data, &data);
            }
            other => panic!("Expected Scrollback, got: {other:?}"),
        }
    }

    #[test]
    fn test_process_exited_round_trip() {
        let frame = Frame::ProcessExited {
            session_uuid: "sess-exit-1".to_string(),
            exit_code: Some(0),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_process_exited_none_round_trip() {
        let frame = Frame::ProcessExited {
            session_uuid: "sess-exit-none".to_string(),
            exit_code: None,
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_multiple_frames_in_single_feed() {
        let f1 = Frame::Json(serde_json::json!({"msg": 1}));
        let f2 = Frame::PtyOutput {
            session_uuid: "sess-0".to_string(),
            data: b"data".to_vec(),
        };
        let f3 = Frame::Json(serde_json::json!({"msg": 2}));

        let mut buf = Vec::new();
        buf.extend_from_slice(&f1.encode());
        buf.extend_from_slice(&f2.encode());
        buf.extend_from_slice(&f3.encode());

        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&buf).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], f1);
        assert_eq!(frames[1], f2);
        assert_eq!(frames[2], f3);
    }

    #[test]
    fn test_partial_frame_reassembly() {
        let frame = Frame::Json(serde_json::json!({"key": "value"}));
        let encoded = frame.encode();

        let mut decoder = FrameDecoder::new();

        // Feed first half
        let mid = encoded.len() / 2;
        let frames = decoder.feed(&encoded[..mid]).unwrap();
        assert_eq!(frames.len(), 0);
        assert!(decoder.has_partial());

        // Feed second half
        let frames = decoder.feed(&encoded[mid..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
        assert!(!decoder.has_partial());
    }

    #[test]
    fn test_byte_at_a_time() {
        let frame = Frame::PtyInput {
            session_uuid: "s".to_string(),
            data: b"x".to_vec(),
        };
        let encoded = frame.encode();

        let mut decoder = FrameDecoder::new();
        for (i, byte) in encoded.iter().enumerate() {
            let frames = decoder.feed(&[*byte]).unwrap();
            if i < encoded.len() - 1 {
                assert_eq!(frames.len(), 0);
            } else {
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0], frame);
            }
        }
    }

    #[test]
    fn test_empty_pty_data() {
        let frame = Frame::PtyOutput {
            session_uuid: "sess-empty".to_string(),
            data: vec![],
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_empty_session_uuid() {
        let frame = Frame::PtyOutput {
            session_uuid: String::new(),
            data: b"data".to_vec(),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_zero_length_rejected() {
        let buf = [0u8; 4]; // length = 0
        let mut decoder = FrameDecoder::new();
        assert!(decoder.feed(&buf).is_err());
    }

    #[test]
    fn test_oversized_frame_rejected() {
        let length = MAX_FRAME_SIZE + 1;
        let buf = length.to_le_bytes();
        let mut decoder = FrameDecoder::new();
        assert!(decoder.feed(&buf).is_err());
    }

    #[test]
    fn test_unknown_frame_type_rejected() {
        let payload = b"test";
        let length = (payload.len() + 1) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&length.to_le_bytes());
        buf.push(0xFF); // Unknown type
        buf.extend_from_slice(payload);

        let mut decoder = FrameDecoder::new();
        assert!(decoder.feed(&buf).is_err());
    }

    #[test]
    fn test_large_pty_data() {
        let data = vec![0x42u8; 256 * 1024]; // 256KB
        let frame = Frame::PtyOutput {
            session_uuid: "sess-large".to_string(),
            data: data.clone(),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        if let Frame::PtyOutput {
            data: decoded_data, ..
        } = &frames[0]
        {
            assert_eq!(decoded_data.len(), data.len());
        } else {
            panic!("Expected PtyOutput");
        }
    }

    #[test]
    fn test_binary_round_trip() {
        let frame = Frame::Binary(b"raw plugin data".to_vec());
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }

    #[test]
    fn test_empty_binary_round_trip() {
        let frame = Frame::Binary(vec![]);
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], frame);
    }
}
