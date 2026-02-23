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
//! - `0x02`: PTY output binary (hub→client) — `[u16 agent][u16 pty][raw bytes]`
//! - `0x03`: PTY input binary (client→hub) — `[u16 agent][u16 pty][raw bytes]`
//! - `0x04`: PTY scrollback (hub→client) — `[u16 agent][u16 pty][u8 kitty][raw bytes]`
//! - `0x05`: Process exited (hub→client) — `[u16 agent][u16 pty][i32 exit_code]`
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
        /// Agent index.
        agent_index: u16,
        /// PTY index within the agent.
        pty_index: u16,
        /// Raw PTY output bytes.
        data: Vec<u8>,
    },

    /// PTY input data (client → hub).
    PtyInput {
        /// Agent index.
        agent_index: u16,
        /// PTY index within the agent.
        pty_index: u16,
        /// Raw keyboard input bytes.
        data: Vec<u8>,
    },

    /// PTY scrollback data (hub → client, sent once on subscribe).
    Scrollback {
        /// Agent index.
        agent_index: u16,
        /// PTY index within the agent.
        pty_index: u16,
        /// Whether kitty keyboard protocol is active.
        kitty_enabled: bool,
        /// Raw scrollback bytes.
        data: Vec<u8>,
    },

    /// PTY process exited (hub → client).
    ProcessExited {
        /// Agent index.
        agent_index: u16,
        /// PTY index within the agent.
        pty_index: u16,
        /// Exit code (None if killed by signal).
        exit_code: Option<i32>,
    },

    /// Raw binary data (no PTY routing header).
    ///
    /// Used by `socket.send_binary()` for plugin-level binary messaging.
    /// Not interpreted as PTY data.
    Binary(Vec<u8>),
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
            Frame::PtyOutput { agent_index, pty_index, data } => {
                let mut payload = Vec::with_capacity(4 + data.len());
                payload.extend_from_slice(&agent_index.to_le_bytes());
                payload.extend_from_slice(&pty_index.to_le_bytes());
                payload.extend_from_slice(data);
                encode_raw(frame_type::PTY_OUTPUT, &payload)
            }
            Frame::PtyInput { agent_index, pty_index, data } => {
                let mut payload = Vec::with_capacity(4 + data.len());
                payload.extend_from_slice(&agent_index.to_le_bytes());
                payload.extend_from_slice(&pty_index.to_le_bytes());
                payload.extend_from_slice(data);
                encode_raw(frame_type::PTY_INPUT, &payload)
            }
            Frame::Scrollback { agent_index, pty_index, kitty_enabled, data } => {
                let mut payload = Vec::with_capacity(5 + data.len());
                payload.extend_from_slice(&agent_index.to_le_bytes());
                payload.extend_from_slice(&pty_index.to_le_bytes());
                payload.push(u8::from(*kitty_enabled));
                payload.extend_from_slice(data);
                encode_raw(frame_type::SCROLLBACK, &payload)
            }
            Frame::ProcessExited { agent_index, pty_index, exit_code } => {
                let mut payload = Vec::with_capacity(8);
                payload.extend_from_slice(&agent_index.to_le_bytes());
                payload.extend_from_slice(&pty_index.to_le_bytes());
                payload.extend_from_slice(&exit_code.unwrap_or(-1).to_le_bytes());
                encode_raw(frame_type::PROCESS_EXITED, &payload)
            }
            Frame::Binary(data) => {
                encode_raw(frame_type::BINARY, data)
            }
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
            let value: serde_json::Value = serde_json::from_slice(payload)
                .map_err(|e| anyhow!("Invalid JSON frame: {e}"))?;
            Ok(Frame::Json(value))
        }
        frame_type::PTY_OUTPUT => {
            if payload.len() < 4 {
                bail!("PTY output frame too short: {} bytes", payload.len());
            }
            let agent_index = u16::from_le_bytes([payload[0], payload[1]]);
            let pty_index = u16::from_le_bytes([payload[2], payload[3]]);
            Ok(Frame::PtyOutput {
                agent_index,
                pty_index,
                data: payload[4..].to_vec(),
            })
        }
        frame_type::PTY_INPUT => {
            if payload.len() < 4 {
                bail!("PTY input frame too short: {} bytes", payload.len());
            }
            let agent_index = u16::from_le_bytes([payload[0], payload[1]]);
            let pty_index = u16::from_le_bytes([payload[2], payload[3]]);
            Ok(Frame::PtyInput {
                agent_index,
                pty_index,
                data: payload[4..].to_vec(),
            })
        }
        frame_type::SCROLLBACK => {
            if payload.len() < 5 {
                bail!("Scrollback frame too short: {} bytes", payload.len());
            }
            let agent_index = u16::from_le_bytes([payload[0], payload[1]]);
            let pty_index = u16::from_le_bytes([payload[2], payload[3]]);
            let kitty_enabled = payload[4] != 0;
            Ok(Frame::Scrollback {
                agent_index,
                pty_index,
                kitty_enabled,
                data: payload[5..].to_vec(),
            })
        }
        frame_type::PROCESS_EXITED => {
            if payload.len() < 8 {
                bail!("Process exited frame too short: {} bytes", payload.len());
            }
            let agent_index = u16::from_le_bytes([payload[0], payload[1]]);
            let pty_index = u16::from_le_bytes([payload[2], payload[3]]);
            let raw_code = i32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
            let exit_code = if raw_code == -1 { None } else { Some(raw_code) };
            Ok(Frame::ProcessExited {
                agent_index,
                pty_index,
                exit_code,
            })
        }
        frame_type::BINARY => {
            Ok(Frame::Binary(payload.to_vec()))
        }
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
            agent_index: 0,
            pty_index: 1,
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
            agent_index: 2,
            pty_index: 0,
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
            agent_index: 0,
            pty_index: 0,
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
    fn test_process_exited_round_trip() {
        let frame = Frame::ProcessExited {
            agent_index: 1,
            pty_index: 0,
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
            agent_index: 0,
            pty_index: 0,
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
        let f2 = Frame::PtyOutput { agent_index: 0, pty_index: 0, data: b"data".to_vec() };
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
            agent_index: 0,
            pty_index: 0,
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
            agent_index: 0,
            pty_index: 0,
            data: vec![],
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
            agent_index: 0,
            pty_index: 0,
            data: data.clone(),
        };
        let encoded = frame.encode();
        let mut decoder = FrameDecoder::new();
        let frames = decoder.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        if let Frame::PtyOutput { data: decoded_data, .. } = &frames[0] {
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
