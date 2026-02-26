//! Broker IPC protocol types and wire encoding.
//!
//! Wire format (identical to `socket/framing.rs`):
//!
//! ```text
//! [u32 LE: payload_len + 1] [u8: frame_type] [payload_bytes]
//! ```
//!
//! Frame types:
//! - `0x10` `HubControl`   — JSON-encoded [`HubMessage`] (Hub → Broker)
//! - `0x11` `BrokerControl`— JSON-encoded [`BrokerMessage`] (Broker → Hub)
//! - `0x12` `PtyInput`     — `[u32 LE session_id][raw bytes]` (Hub → Broker)
//! - `0x13` `PtyOutput`    — `[u32 LE session_id][raw bytes]` (Broker → Hub)
//! - `0x14` `Snapshot`     — `[u32 LE session_id][raw bytes]` (Broker → Hub, GetSnapshot response)
//! - `0x15` `FdTransfer`   — registration payload (Hub → Broker); master PTY FD arrives in
//!                           the same `sendmsg()` call via SCM_RIGHTS ancillary data.
//!
//! ## Session lifecycle
//!
//! 1. Hub opens PTY → calls `sendmsg` with an `FdTransfer` frame + FD in ancillary data.
//! 2. Broker receives FD, registers session, returns [`BrokerMessage::Registered`] with a
//!    u32 `session_id`.
//! 3. Hub addresses all subsequent input via `session_id` in `PtyInput` frames.
//! 4. Broker forwards PTY output via `PtyOutput` frames (same `session_id` prefix).
//! 5. On Hub disconnect: broker starts configurable timeout.
//!    - Reconnect within window → Hub calls [`HubMessage::GetSnapshot`] per session, broker
//!      sends ring-buffer contents in a `Snapshot` frame, Hub replays into fresh
//!      `vt100::Parser`.
//!    - Timeout expires → broker kills children and sends [`BrokerMessage::Timeout`] (if
//!      still connected) then exits.

// Rust guideline compliant 2026-02

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

/// Maximum frame payload size (16 MB — same cap as `socket/framing.rs`).
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

// ─── Frame type constants ──────────────────────────────────────────────────

/// Frame type byte constants for the broker wire protocol.
pub mod frame_type {
    /// JSON-encoded [`super::HubMessage`] (Hub → Broker).
    pub const HUB_CONTROL: u8 = 0x10;
    /// JSON-encoded [`super::BrokerMessage`] (Broker → Hub).
    pub const BROKER_CONTROL: u8 = 0x11;
    /// Raw PTY input: `[u32 LE session_id][bytes]` (Hub → Broker).
    pub const PTY_INPUT: u8 = 0x12;
    /// Raw PTY output: `[u32 LE session_id][bytes]` (Broker → Hub).
    pub const PTY_OUTPUT: u8 = 0x13;
    /// Ring-buffer snapshot: `[u32 LE session_id][bytes]` (Broker → Hub).
    pub const SNAPSHOT: u8 = 0x14;
    /// FD transfer registration (Hub → Broker); master PTY FD in SCM_RIGHTS.
    pub const FD_TRANSFER: u8 = 0x15;
}

// ─── Control message enums ─────────────────────────────────────────────────

/// Messages sent from Hub to Broker in `HubControl` frames (JSON payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HubMessage {
    /// Request the raw ring-buffer snapshot for a registered session.
    ///
    /// Broker replies with a `Snapshot` binary frame (same `session_id`).
    GetSnapshot {
        /// Opaque session identifier returned by `BrokerMessage::Registered`.
        session_id: u32,
    },

    /// Resize a PTY session via `ioctl(TIOCSWINSZ)`.
    ResizePty {
        /// Opaque session identifier.
        session_id: u32,
        /// New terminal row count.
        rows: u16,
        /// New terminal column count.
        cols: u16,
    },

    /// Unregister a session whose process has already exited cleanly.
    ///
    /// Broker closes the FD and discards the ring buffer.
    UnregisterPty {
        /// Opaque session identifier.
        session_id: u32,
    },

    /// Configure the reconnect timeout window.
    ///
    /// Hub sends this immediately after establishing the connection.
    /// Broker starts this countdown when the Hub connection drops.
    SetTimeout {
        /// Timeout in seconds.
        seconds: u64,
    },

    /// Immediate shutdown: kill all PTY children and exit.
    ///
    /// Hub sends this before a clean restart so no orphans linger.
    KillAll,

    /// Keepalive.
    Ping,
}

/// Messages sent from Broker to Hub in `BrokerControl` frames (JSON payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrokerMessage {
    /// Returned after a successful `FdTransfer`.
    ///
    /// `session_id` is the u32 token the Hub must use for all subsequent
    /// `PtyInput` frames and control messages targeting this PTY.
    Registered {
        /// Agent key that owns this session.
        agent_key: String,
        /// PTY index within the agent (0 = CLI, 1 = server, …).
        pty_index: usize,
        /// Opaque session identifier assigned by the broker.
        session_id: u32,
    },

    /// Generic acknowledgment (SetTimeout, UnregisterPty).
    Ack,

    /// Pong in response to `Ping`.
    Pong,

    /// A tracked PTY process has exited.
    PtyExited {
        /// Opaque session identifier.
        session_id: u32,
        /// Agent key that owned the session.
        agent_key: String,
        /// PTY index within the agent.
        pty_index: usize,
        /// `None` if killed by signal.
        exit_code: Option<i32>,
    },

    /// Reconnect timeout expired — broker is shutting down.
    Timeout,

    /// Error during a Hub-requested operation.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

// ─── FdTransfer payload ────────────────────────────────────────────────────

/// Payload carried in an `FdTransfer` frame (type `0x15`).
///
/// The master PTY FD itself arrives as SCM_RIGHTS ancillary data in the same
/// `sendmsg()` call.  This payload provides the metadata needed to register
/// the session without a separate round-trip.
///
/// Wire layout:
/// ```text
/// [u8: key_len] [key_bytes…] [u8: pty_index] [u32 LE: child_pid]
/// [u16 LE: rows] [u16 LE: cols]
/// ```
#[derive(Debug, Clone)]
pub struct FdTransferPayload {
    /// Agent key identifying this session in the Hub.
    pub agent_key: String,
    /// PTY index within the agent (0 = CLI, 1 = server, …).
    pub pty_index: usize,
    /// PID of the child process for monitoring and SIGKILL on timeout.
    pub child_pid: u32,
    /// Initial terminal row count.
    pub rows: u16,
    /// Initial terminal column count.
    pub cols: u16,
}

impl FdTransferPayload {
    /// Encode to wire bytes.
    pub fn encode(&self) -> Vec<u8> {
        let key = self.agent_key.as_bytes();
        let mut buf = Vec::with_capacity(1 + key.len() + 1 + 4 + 2 + 2);
        buf.push(key.len() as u8);
        buf.extend_from_slice(key);
        buf.push(self.pty_index as u8);
        buf.extend_from_slice(&self.child_pid.to_le_bytes());
        buf.extend_from_slice(&self.rows.to_le_bytes());
        buf.extend_from_slice(&self.cols.to_le_bytes());
        buf
    }

    /// Decode from wire bytes.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        if payload.is_empty() {
            bail!("FdTransfer payload is empty");
        }
        let key_len = payload[0] as usize;
        let min_len = 1 + key_len + 1 + 4 + 2 + 2;
        if payload.len() < min_len {
            bail!(
                "FdTransfer payload too short: {} bytes, expected >= {}",
                payload.len(),
                min_len
            );
        }
        let agent_key =
            std::str::from_utf8(&payload[1..1 + key_len])
                .map_err(|e| anyhow!("FdTransfer agent_key is not UTF-8: {e}"))?
                .to_owned();
        let mut off = 1 + key_len;
        let pty_index = payload[off] as usize;
        off += 1;
        let child_pid = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]);
        off += 4;
        let rows = u16::from_le_bytes([payload[off], payload[off+1]]);
        off += 2;
        let cols = u16::from_le_bytes([payload[off], payload[off+1]]);
        Ok(Self { agent_key, pty_index, child_pid, rows, cols })
    }
}

// ─── Frame encoding helpers ────────────────────────────────────────────────

/// Encode a JSON control message into a wire frame.
pub fn encode_control<T: Serialize>(frame_type: u8, msg: &T) -> Vec<u8> {
    let payload = serde_json::to_vec(msg).expect("broker message serialization cannot fail");
    encode_raw(frame_type, &payload)
}

/// Encode a Hub→Broker control message.
pub fn encode_hub_control(msg: &HubMessage) -> Vec<u8> {
    encode_control(frame_type::HUB_CONTROL, msg)
}

/// Encode a Broker→Hub control message.
pub fn encode_broker_control(msg: &BrokerMessage) -> Vec<u8> {
    encode_control(frame_type::BROKER_CONTROL, msg)
}

/// Encode a binary data frame with a `session_id` routing prefix.
///
/// Layout: `[u32 LE session_id][raw bytes]`.
pub fn encode_data(frame_type: u8, session_id: u32, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + data.len());
    payload.extend_from_slice(&session_id.to_le_bytes());
    payload.extend_from_slice(data);
    encode_raw(frame_type, &payload)
}

/// Encode an `FdTransfer` frame (without the ancillary FD — that is
/// sent separately via `sendmsg`).
pub fn encode_fd_transfer(reg: &FdTransferPayload) -> Vec<u8> {
    encode_raw(frame_type::FD_TRANSFER, &reg.encode())
}

fn encode_raw(ft: u8, payload: &[u8]) -> Vec<u8> {
    let length = (payload.len() + 1) as u32; // +1 for the type byte
    let mut buf = Vec::with_capacity(4 + 1 + payload.len());
    buf.extend_from_slice(&length.to_le_bytes());
    buf.push(ft);
    buf.extend_from_slice(payload);
    buf
}

// ─── Frame decoder ─────────────────────────────────────────────────────────

/// A decoded broker protocol frame.
#[derive(Debug)]
pub enum BrokerFrame {
    /// JSON-encoded [`HubMessage`].
    HubControl(HubMessage),
    /// JSON-encoded [`BrokerMessage`].
    BrokerControl(BrokerMessage),
    /// PTY input data: (session_id, bytes).
    PtyInput(u32, Vec<u8>),
    /// PTY output data: (session_id, bytes).
    PtyOutput(u32, Vec<u8>),
    /// Snapshot data: (session_id, bytes).
    Snapshot(u32, Vec<u8>),
    /// FD transfer registration payload (FD arrives via SCM_RIGHTS separately).
    FdTransfer(FdTransferPayload),
}

/// Incremental frame decoder — same byte-accumulation design as
/// `socket::framing::FrameDecoder`.
#[derive(Debug, Default)]
pub struct BrokerFrameDecoder {
    buf: Vec<u8>,
}

impl BrokerFrameDecoder {
    /// Create a new decoder with an empty buffer.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Feed bytes and extract all complete frames.
    ///
    /// Incomplete data is retained for the next call.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<BrokerFrame>> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();

        loop {
            if self.buf.len() < 4 {
                break;
            }
            let length = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
            if length == 0 {
                bail!("broker frame: zero length");
            }
            if length > MAX_FRAME_SIZE {
                bail!("broker frame too large: {length} bytes");
            }
            let total = 4 + length as usize;
            if self.buf.len() < total {
                break;
            }

            let ft = self.buf[4];
            let payload = &self.buf[5..total];
            let frame = decode_frame(ft, payload)?;
            frames.push(frame);
            self.buf.drain(..total);
        }

        Ok(frames)
    }
}

fn decode_frame(ft: u8, payload: &[u8]) -> Result<BrokerFrame> {
    match ft {
        frame_type::HUB_CONTROL => {
            let msg: HubMessage = serde_json::from_slice(payload)
                .map_err(|e| anyhow!("invalid HubControl JSON: {e}"))?;
            Ok(BrokerFrame::HubControl(msg))
        }
        frame_type::BROKER_CONTROL => {
            let msg: BrokerMessage = serde_json::from_slice(payload)
                .map_err(|e| anyhow!("invalid BrokerControl JSON: {e}"))?;
            Ok(BrokerFrame::BrokerControl(msg))
        }
        frame_type::PTY_INPUT => {
            if payload.len() < 4 {
                bail!("PtyInput frame too short");
            }
            let session_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            Ok(BrokerFrame::PtyInput(session_id, payload[4..].to_vec()))
        }
        frame_type::PTY_OUTPUT => {
            if payload.len() < 4 {
                bail!("PtyOutput frame too short");
            }
            let session_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            Ok(BrokerFrame::PtyOutput(session_id, payload[4..].to_vec()))
        }
        frame_type::SNAPSHOT => {
            if payload.len() < 4 {
                bail!("Snapshot frame too short");
            }
            let session_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            Ok(BrokerFrame::Snapshot(session_id, payload[4..].to_vec()))
        }
        frame_type::FD_TRANSFER => {
            let reg = FdTransferPayload::decode(payload)?;
            Ok(BrokerFrame::FdTransfer(reg))
        }
        _ => bail!("unknown broker frame type: 0x{ft:02x}"),
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_control_round_trip() {
        let msg = HubMessage::ResizePty { session_id: 3, rows: 24, cols: 80 };
        let encoded = encode_hub_control(&msg);
        let mut dec = BrokerFrameDecoder::new();
        let frames = dec.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        matches!(&frames[0], BrokerFrame::HubControl(HubMessage::ResizePty { session_id: 3, .. }));
    }

    #[test]
    fn broker_control_round_trip() {
        let msg = BrokerMessage::Registered {
            agent_key: "my-agent".into(),
            pty_index: 0,
            session_id: 42,
        };
        let encoded = encode_broker_control(&msg);
        let mut dec = BrokerFrameDecoder::new();
        let frames = dec.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        matches!(&frames[0], BrokerFrame::BrokerControl(BrokerMessage::Registered { session_id: 42, .. }));
    }

    #[test]
    fn pty_input_round_trip() {
        let encoded = encode_data(frame_type::PTY_INPUT, 7, b"hello");
        let mut dec = BrokerFrameDecoder::new();
        let frames = dec.feed(&encoded).unwrap();
        assert_eq!(frames.len(), 1);
        if let BrokerFrame::PtyInput(sid, data) = &frames[0] {
            assert_eq!(*sid, 7);
            assert_eq!(data, b"hello");
        } else {
            panic!("expected PtyInput");
        }
    }

    #[test]
    fn fd_transfer_payload_round_trip() {
        let reg = FdTransferPayload {
            agent_key: "test-agent".into(),
            pty_index: 1,
            child_pid: 12345,
            rows: 24,
            cols: 80,
        };
        let encoded = reg.encode();
        let decoded = FdTransferPayload::decode(&encoded).unwrap();
        assert_eq!(decoded.agent_key, "test-agent");
        assert_eq!(decoded.pty_index, 1);
        assert_eq!(decoded.child_pid, 12345);
        assert_eq!(decoded.rows, 24);
        assert_eq!(decoded.cols, 80);
    }

    #[test]
    fn partial_reassembly() {
        let encoded = encode_data(frame_type::PTY_OUTPUT, 1, b"data");
        let mid = encoded.len() / 2;
        let mut dec = BrokerFrameDecoder::new();
        assert!(dec.feed(&encoded[..mid]).unwrap().is_empty());
        let frames = dec.feed(&encoded[mid..]).unwrap();
        assert_eq!(frames.len(), 1);
    }
}
