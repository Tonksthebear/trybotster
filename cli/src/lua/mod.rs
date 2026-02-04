//! Lua scripting runtime for the botster hub.
//!
//! This module provides Lua scripting support for hot-reloadable behavior
//! customization. The Lua runtime is initialized at hub startup and can
//! load scripts from the filesystem.
//!
//! # Architecture
//!
//! ```text
//! Hub
//!  └── LuaRuntime
//!       ├── Lua state (mlua)
//!       ├── FileWatcher (hot-reload)
//!       └── Primitives
//!            ├── log (info, warn, error, debug)
//!            ├── webrtc (peer events, messaging)
//!            ├── pty (terminal operations)
//!            ├── hub (state queries, agent ops)
//!            └── events (lifecycle subscriptions)
//! ```
//!
//! # Configuration
//!
//! - `BOTSTER_LUA_PATH` - Override default script path (`~/.botster/lua`)
//! - `BOTSTER_LUA_STRICT` - If "1", panic on Lua errors instead of logging
//!
//! # Hot-Reload
//!
//! When file watching is enabled, the runtime monitors the Lua script directory
//! for changes. Modified files are automatically reloaded via the Lua `loader`
//! module. Core modules (`core.state`, `core.hooks`, `core.loader`) are protected
//! and cannot be reloaded - their state persists across reloads.
//!
//! # Usage
//!
//! ```ignore
//! let lua = LuaRuntime::new()?;
//! lua.load_file(Path::new("init.lua"))?;
//! lua.call_function("my_handler", ())?;
//!
//! // Hot-reload support
//! lua.start_file_watching()?;
//! // In event loop:
//! lua.poll_and_reload();
//! ```

pub mod embedded;
pub mod file_watcher;
pub mod primitives;
pub mod runtime;

pub use file_watcher::LuaFileWatcher;
pub use primitives::{
    // PTY primitives
    CreateForwarderRequest, PtyForwarder, PtyOutputContext, PtyRequest, PtyRequestQueue,
    // WebRTC primitives
    WebRtcSendQueue, WebRtcSendRequest,
    // Hub state primitives
    HubRequest, HubRequestQueue,
    // Event system primitives
    EventCallbackId, EventCallbacks, SharedEventCallbacks,
};
pub use runtime::LuaRuntime;
