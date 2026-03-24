//! Hub-side connection to a per-session process.
//!
//! Each `SessionConnection` owns a Unix socket stream to one session process.
//! Unlike the broker's multiplexed `BrokerConnection`, there's no demux thread —
//! each connection has its own reader task that produces `HubEvent` variants directly.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::protocol::*;
use super::SpawnConfig;

/// Shared session connection, same pattern as `SharedBrokerConnection`.
pub type SharedSessionConnection = Arc<Mutex<Option<SessionConnection>>>;

/// Hub-side connection to a single session process.
pub struct SessionConnection {
    stream: UnixStream,
    decoder: FrameDecoder,
    /// Protocol version negotiated during handshake.
    pub protocol_version: u8,
    /// Session metadata received during handshake.
    pub metadata: SessionMetadata,
}

impl SessionConnection {
    /// Connect to a session process socket and perform handshake.
    pub fn connect(socket_path: &Path) -> Result<Self> {
        let mut stream = UnixStream::connect(socket_path)
            .with_context(|| format!("connect to session: {}", socket_path.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("set session socket read timeout")?;

        let (version, metadata) =
            handshake_hub(&mut stream).context("session handshake")?;

        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            protocol_version: version,
            metadata,
        })
    }

    /// Send spawn configuration to the session process.
    ///
    /// Must be called immediately after handshake for new sessions.
    /// The session process reads this as its first frame and uses it
    /// to create the PTY and spawn the child.
    pub fn send_spawn_config(&mut self, config: &SpawnConfig) -> Result<()> {
        let frame = encode_json(FRAME_PTY_INPUT, config)?;
        self.stream
            .write_all(&frame)
            .context("send spawn config")?;
        self.stream.flush().context("flush spawn config")?;
        Ok(())
    }

    /// Write raw PTY input bytes.
    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        let frame = encode_frame(FRAME_PTY_INPUT, data);
        self.stream
            .write_all(&frame)
            .context("send PTY input")?;
        Ok(())
    }

    /// Send a resize command.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let frame = encode_json(FRAME_RESIZE, &serde_json::json!({"rows": rows, "cols": cols}))?;
        self.stream.write_all(&frame).context("send resize")?;
        Ok(())
    }

    /// Request and receive an ANSI snapshot.
    pub fn get_snapshot(&mut self) -> Result<Vec<u8>> {
        let req = encode_empty(FRAME_GET_SNAPSHOT);
        self.stream
            .write_all(&req)
            .context("send GetSnapshot")?;
        self.stream.flush()?;

        let frame = self.read_response(FRAME_SNAPSHOT)?;
        Ok(frame.payload)
    }

    /// Request and receive plain text screen contents.
    pub fn get_screen(&mut self) -> Result<String> {
        let req = encode_empty(FRAME_GET_SCREEN);
        self.stream
            .write_all(&req)
            .context("send GetScreen")?;
        self.stream.flush()?;

        let frame = self.read_response(FRAME_SCREEN)?;
        String::from_utf8(frame.payload).context("screen text not UTF-8")
    }

    /// Request and receive terminal mode flags.
    pub fn get_mode_flags(&mut self) -> Result<ModeFlags> {
        let req = encode_empty(FRAME_GET_MODE_FLAGS);
        self.stream
            .write_all(&req)
            .context("send GetModeFlags")?;
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
        self.stream
            .write_all(&req)
            .context("send shutdown")?;
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

    /// Clone the underlying stream for a reader task.
    ///
    /// The reader task owns this clone and reads PTY output frames.
    /// The original stream is used for writes (input, resize, RPCs).
    pub fn try_clone_for_reader(&self) -> Result<UnixStream> {
        self.stream.try_clone().context("clone session socket for reader")
    }

    /// Read the next response frame of the expected type.
    ///
    /// Skips `PtyOutput` and `ProcessExited` frames (async events that
    /// should be handled by the reader task, not RPC callers).
    fn read_response(&mut self, expected_type: u8) -> Result<Frame> {
        let mut buf = [0u8; 8192];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

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
                // Skip async frames — these belong to the reader task
                if frame.frame_type == FRAME_PTY_OUTPUT
                    || frame.frame_type == FRAME_PROCESS_EXITED
                {
                    continue;
                }
                if frame.frame_type == expected_type {
                    return Ok(frame);
                }
                // Unexpected frame type — log and skip
                log::debug!(
                    "[session-conn] unexpected frame 0x{:02x} while waiting for 0x{:02x}",
                    frame.frame_type,
                    expected_type
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_session_connection_type_compiles() {
        let _conn: SharedSessionConnection = Arc::new(Mutex::new(None));
    }
}
