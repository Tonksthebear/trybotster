//! Unix domain socket IPC for hub↔client communication.
//!
//! Enables TUI detach/reattach (like tmux) and plugin access via a
//! transport-agnostic socket protocol. Socket clients go through the same
//! Lua `Client` abstraction as browser/WebRTC clients.
//!
//! # Architecture
//!
//! ```text
//! Hub Process                          Client Process (botster attach)
//! ┌──────────────────┐                ┌──────────────────┐
//! │ SocketServer     │                │ TuiBridge        │
//! │  UnixListener    │◄──────────────►│  UnixStream      │
//! │  SocketClientConn│  frames over   │  mpsc↔frame xlat │
//! │  per connection  │  Unix socket   │                  │
//! └────────┬─────────┘                └────────┬─────────┘
//!          │ HubEvent                          │ TuiRequest/TuiOutput
//!          ▼                                   ▼
//!       Hub event loop                     TuiRunner
//! ```
//!
//! # Wire Protocol
//!
//! Length-prefixed frames: `[u32 LE length][u8 type][payload]`
//!
//! See [`framing`] for frame types and codec.

pub mod client_conn;
pub mod framing;
pub mod server;
pub mod tui_bridge;
