//! Per-plugin worker contract.
//!
//! Plugin workers own plugin execution. The hub may keep descriptor registries
//! for routing and UI discovery, but executable plugin code should cross this
//! boundary by stable handler reference and bounded mailbox request.

use std::path::PathBuf;

use super::{BoundedQueueConfig, RequestId};

/// Stable key for a loaded plugin instance.
pub type PluginKey = String;

/// Default bounded mailbox config for a single plugin worker.
pub const PLUGIN_WORKER_QUEUE: BoundedQueueConfig = BoundedQueueConfig::new("worker.plugin", 512);

/// Plugin capability families that can own executable handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginHandlerKind {
    /// `lib.action` UI action handler.
    UiAction,
    /// `lib.session_actions` executable session action.
    SessionAction,
    /// `hub.hooks` observer or interceptor.
    Hook,
    /// `lib.commands` command handler.
    Command,
    /// Plugin-owned timer callback.
    Timer,
    /// `events.on` callback.
    Event,
    /// `watch.directory` callback.
    Watch,
    /// `lib.mcp` tool, prompt, or resource handler.
    Mcp,
    /// `lib.surfaces` route renderer.
    SurfaceRoute,
    /// `lib.plugin_assets` message handler.
    AssetMessage,
}

/// Stable reference to a plugin-owned handler inside its worker runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHandlerRef {
    /// Capability family that owns the handler.
    pub kind: PluginHandlerKind,
    /// Semantic id within the capability family.
    pub id: String,
    /// Optional named handler/route when one id can have multiple handlers.
    pub name: Option<String>,
}

/// Source location and identity for a plugin worker runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLoadSpec {
    /// Stable plugin key used by hub registries and reload/unload.
    pub plugin_key: PluginKey,
    /// User-facing plugin display name.
    pub display_name: String,
    /// Absolute path to the plugin init file.
    pub init_path: PathBuf,
    /// Loader source, for example `device` or `repo`.
    pub source: Option<String>,
    /// Repository root for repo-scoped plugins.
    pub repo_root: Option<PathBuf>,
}

/// Messages accepted by one plugin worker.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginWorkerMessage {
    /// Load or replace the plugin runtime from disk.
    Load {
        /// Plugin source and identity.
        spec: PluginLoadSpec,
    },
    /// Invoke a registered plugin handler.
    Invoke {
        /// Request correlation id.
        request_id: RequestId,
        /// Handler to execute inside the plugin worker.
        handler: PluginHandlerRef,
        /// JSON-compatible payload owned by the caller/registry.
        payload: serde_json::Value,
        /// Execution timeout for this handler invocation.
        timeout_ms: u64,
    },
    /// Shut the worker down.
    Shutdown {
        /// Human-readable shutdown reason for diagnostics.
        reason: String,
    },
}

/// Events emitted by one plugin worker back to hub-owned orchestration.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginWorkerEvent {
    /// Plugin loaded and registered its descriptors.
    Loaded {
        /// Loaded plugin key.
        plugin_key: PluginKey,
    },
    /// Plugin load failed before becoming active.
    LoadFailed {
        /// Plugin key that failed.
        plugin_key: PluginKey,
        /// Failure detail.
        error: String,
    },
    /// Handler invocation completed.
    InvokeCompleted {
        /// Request correlation id.
        request_id: RequestId,
        /// JSON-compatible return value.
        result: serde_json::Value,
    },
    /// Handler invocation failed or timed out.
    InvokeFailed {
        /// Request correlation id.
        request_id: RequestId,
        /// Failure detail.
        error: String,
    },
    /// Worker mailbox rejected work due to pressure.
    Backpressure {
        /// Plugin key whose mailbox is saturated.
        plugin_key: PluginKey,
        /// Queue capacity.
        capacity: usize,
    },
    /// Worker exited.
    Stopped {
        /// Plugin key that stopped.
        plugin_key: PluginKey,
        /// Human-readable reason.
        reason: String,
    },
}
