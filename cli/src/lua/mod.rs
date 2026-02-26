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
//!       └── Primitives
//!            ├── log (info, warn, error, debug)
//!            ├── webrtc (peer events, messaging)
//!            ├── pty (terminal operations)
//!            ├── hub (state queries, agent ops)
//!            ├── worktree (git worktree queries, create, delete)
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
//! Hot-reload is handled entirely by Lua's `handlers/module_watcher.lua`
//! using the `watch.directory()` primitive with poll mode. It monitors both
//! core Lua modules and plugin directories, reloading via `loader.reload()`.
//! Hub modules (`hub.state`, `hub.hooks`, `hub.loader`) are protected
//! and cannot be reloaded - their state persists across reloads.
//!
//! # Usage
//!
//! ```ignore
//! let lua = LuaRuntime::new()?;
//! lua.load_file(Path::new("init.lua"))?;
//! lua.call_function("my_handler", ())?;
//!
//! // Hot-reload is handled by Lua's handlers/module_watcher.lua
//! // using the watch.directory() primitive.
//! ```

pub mod embedded;
pub mod primitives;
pub mod runtime;

pub use primitives::{
    // PTY primitives
    CreateForwarderRequest, PtyForwarder, PtyOutputContext, PtyRequest,
    // WebRTC primitives
    WebRtcSendRequest,
    // Hub state primitives
    HubRequest,
    // Worktree primitives
    WorktreeRequest,
    // Event system primitives
    EventCallbackId, EventCallbacks, SharedEventCallbacks,
};
pub use runtime::LuaRuntime;
