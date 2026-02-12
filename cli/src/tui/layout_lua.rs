//! TUI-owned Lua state for declarative layout rendering and keybinding dispatch.
//!
//! Creates a separate, lightweight `mlua::Lua` state owned by TuiRunner's thread.
//! This avoids threading issues (the Hub's LuaRuntime is `!Send`).
//!
//! Loads three Lua modules:
//! - `ui/layout.lua` — calls `render(state)` and `render_overlay(state)` each frame
//! - `ui/keybindings.lua` — calls `handle_key(descriptor, mode, context)` per keypress
//! - `ui/actions.lua` — calls `on_action(action, context)` for compound workflow dispatch

use anyhow::{anyhow, Result};
use mlua::{Lua, Table as LuaTable, Value as LuaValue};

use super::render::RenderContext;
use super::render_tree::RenderNode;
use crate::compat::VpnStatus;

/// Action returned by Lua `handle_key()`.
///
/// Lua returns a table `{ action = "name", ... }` or `nil`. This struct
/// captures the action name plus optional extra fields for parameterized
/// actions (e.g., `menu_select` with an `index`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaKeyAction {
    /// Action name (e.g., `"open_menu"`, `"input_char"`).
    pub action: String,
    /// Optional character for `input_char` action.
    pub char: Option<char>,
    /// Optional index for `menu_select` action.
    pub index: Option<usize>,
}

/// Context passed to Lua `handle_key()` for mode-specific logic.
///
/// Contains state that keybinding fallback logic needs (e.g., list item
/// count for number shortcut validation).
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    /// Currently selected overlay list item index.
    pub list_selected: usize,
    /// Total number of selectable list items.
    pub list_count: usize,
    /// Terminal height in rows (for scroll amount calculation).
    pub terminal_rows: u16,
}

/// Context passed to Lua `actions.on_action()` for workflow dispatch.
///
/// Contains all state that Lua needs to decide what compound operations
/// to return for application-specific actions.
#[derive(Debug, Clone, Default)]
pub struct ActionContext {
    /// Current mode string.
    pub mode: String,
    /// Current text input buffer contents.
    pub input_buffer: String,
    /// Currently selected overlay list item index.
    pub list_selected: usize,
    /// Action strings from the overlay list (extracted from render tree).
    pub overlay_actions: Vec<String>,
    /// Generic key-value store for in-progress operations.
    pub pending_fields: std::collections::HashMap<String, String>,
    /// Currently selected agent ID (if any).
    pub selected_agent: Option<String>,
    /// Available worktrees as (path, branch) pairs.
    pub available_worktrees: Vec<(String, String)>,
    /// Agent IDs and session counts for navigation computation.
    pub agents: Vec<ActionContextAgent>,
    /// Index of the currently selected agent (0-based).
    pub selected_agent_index: Option<usize>,
    /// Currently active PTY index within the selected agent.
    pub active_pty_index: usize,
}

/// Minimal agent info for Lua action context navigation.
#[derive(Debug, Clone)]
pub struct ActionContextAgent {
    /// Agent ID string.
    pub id: String,
    /// Number of PTY sessions this agent has.
    pub session_count: usize,
}

/// TUI-owned Lua state for layout rendering and keybinding dispatch.
///
/// Wraps a `mlua::Lua` instance with layout and keybinding functions loaded.
/// Owned by TuiRunner's thread — no Send/Sync requirements.
#[derive(Debug)]
pub struct LayoutLua {
    lua: Lua,
    /// Whether keybindings module is loaded (handle_key available).
    keybindings_loaded: bool,
    /// Whether actions module is loaded (on_action available).
    actions_loaded: bool,
    /// Whether events module is loaded (on_hub_event available).
    events_loaded: bool,
}

impl LayoutLua {
    /// Create a new layout Lua state and load the given source.
    ///
    /// # Arguments
    ///
    /// * `lua_source` - Lua source code defining `render(state)` and `render_overlay(state)`
    pub fn new(lua_source: &str) -> Result<Self> {
        let lua = Lua::new();
        lua.load(lua_source)
            .exec()
            .map_err(|e| anyhow!("Failed to load layout Lua: {e}"))?;
        Ok(Self {
            lua,
            keybindings_loaded: false,
            actions_loaded: false,
            events_loaded: false,
        })
    }

    /// Reload the layout from new source (for hot-reload).
    pub fn reload(&self, lua_source: &str) -> Result<()> {
        self.lua
            .load(lua_source)
            .exec()
            .map_err(|e| anyhow!("Failed to reload layout Lua: {e}"))
    }

    /// Load an extension chunk into the TUI Lua state.
    ///
    /// Executes after built-in modules. Extension code can reference,
    /// wrap, or replace globals set by previously loaded modules.
    pub fn load_extension(&self, source: &str, name: &str) -> Result<()> {
        self.lua
            .load(source)
            .set_name(name)
            .exec()
            .map_err(|e| anyhow!("Failed to load UI extension '{name}': {e}"))
    }

    /// Call Lua `render(state)` and return the render tree.
    pub fn call_render(&self, ctx: &RenderContext) -> Result<RenderNode> {
        let state = render_context_to_lua(&self.lua, ctx)?;

        let globals = self.lua.globals();
        let render_fn: mlua::Function = globals
            .get("render")
            .map_err(|e| anyhow!("Lua render() function not found: {e}"))?;

        let result: LuaTable = render_fn
            .call(state)
            .map_err(|e| anyhow!("Lua render() failed: {e}"))?;

        RenderNode::from_lua_table(&result)
    }

    /// Call Lua `render_overlay(state)` and return optional overlay tree.
    pub fn call_render_overlay(&self, ctx: &RenderContext) -> Result<Option<RenderNode>> {
        let state = render_context_to_lua(&self.lua, ctx)?;

        let globals = self.lua.globals();
        let render_overlay_fn: mlua::Function = match globals.get("render_overlay") {
            Ok(f) => f,
            Err(_) => return Ok(None), // No overlay function defined
        };

        let result: LuaValue = render_overlay_fn
            .call(state)
            .map_err(|e| anyhow!("Lua render_overlay() failed: {e}"))?;

        match result {
            LuaValue::Nil => Ok(None),
            LuaValue::Table(table) => {
                let node = RenderNode::from_lua_table(&table)?;
                Ok(Some(node))
            }
            _ => Err(anyhow!(
                "render_overlay() must return a table or nil, got {:?}",
                result
            )),
        }
    }

    /// Call Lua `initial_mode()` to get the boot mode string.
    ///
    /// Returns the mode Lua wants the TUI to start in. Falls back to
    /// empty string if the function isn't defined.
    pub fn call_initial_mode(&self) -> String {
        let globals = self.lua.globals();
        let Ok(func) = globals.get::<mlua::Function>("initial_mode") else {
            return String::new();
        };
        func.call::<String>(()).unwrap_or_default()
    }

    // === Keybinding Support ===

    /// Load the keybindings Lua module.
    ///
    /// Executes the source and stores the returned module table as a global
    /// `_keybindings` so `call_handle_key()` can call `handle_key()` on it.
    pub fn load_keybindings(&mut self, lua_source: &str) -> Result<()> {
        let chunk = self
            .lua
            .load(lua_source)
            .eval::<LuaTable>()
            .map_err(|e| anyhow!("Failed to load keybindings Lua: {e}"))?;

        self.lua
            .globals()
            .set("_keybindings", chunk)
            .map_err(|e| anyhow!("Failed to store keybindings module: {e}"))?;

        self.keybindings_loaded = true;
        Ok(())
    }

    /// Reload the keybindings from new source (for hot-reload).
    pub fn reload_keybindings(&mut self, lua_source: &str) -> Result<()> {
        self.load_keybindings(lua_source)
    }

    /// Whether keybindings are loaded and available.
    #[must_use]
    pub fn has_keybindings(&self) -> bool {
        self.keybindings_loaded
    }

    /// Load the Lua actions module (`actions.lua`).
    ///
    /// The module is stored as `_actions` global so `call_on_action()` can
    /// invoke `on_action()` on it.
    pub fn load_actions(&mut self, lua_source: &str) -> Result<()> {
        let chunk = self
            .lua
            .load(lua_source)
            .eval::<LuaTable>()
            .map_err(|e| anyhow!("Failed to load actions Lua: {e}"))?;

        self.lua
            .globals()
            .set("_actions", chunk)
            .map_err(|e| anyhow!("Failed to store actions module: {e}"))?;

        self.actions_loaded = true;
        Ok(())
    }

    /// Reload the actions module from new source (for hot-reload).
    pub fn reload_actions(&mut self, lua_source: &str) -> Result<()> {
        self.load_actions(lua_source)
    }

    /// Whether actions module is loaded and available.
    #[must_use]
    pub fn has_actions(&self) -> bool {
        self.actions_loaded
    }

    /// Load the Lua events module (`events.lua`).
    ///
    /// The module is stored as `_events` global so `call_on_hub_event()` can
    /// invoke `on_hub_event()` on it.
    pub fn load_events(&mut self, lua_source: &str) -> Result<()> {
        let chunk = self
            .lua
            .load(lua_source)
            .eval::<LuaTable>()
            .map_err(|e| anyhow!("Failed to load events Lua: {e}"))?;

        self.lua
            .globals()
            .set("_events", chunk)
            .map_err(|e| anyhow!("Failed to store events module: {e}"))?;

        self.events_loaded = true;
        Ok(())
    }

    /// Reload the events module from new source (for hot-reload).
    pub fn reload_events(&mut self, lua_source: &str) -> Result<()> {
        self.load_events(lua_source)
    }

    /// Whether events module is loaded and available.
    #[must_use]
    pub fn has_events(&self) -> bool {
        self.events_loaded
    }

    /// Call Lua `actions.on_action(action, context)`.
    ///
    /// Returns `Ok(Some(ops))` if Lua returned a list of compound ops,
    /// `Ok(None)` if Lua returned `nil` (action handled generically by Rust).
    ///
    /// # Arguments
    ///
    /// * `action` - Action name string from keybindings
    /// * `context` - Action context (mode, input_buffer, selected items, etc.)
    pub fn call_on_action(
        &self,
        action: &str,
        context: &ActionContext,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        if !self.actions_loaded {
            return Ok(None);
        }

        let globals = self.lua.globals();
        let actions_module: LuaTable = globals
            .get("_actions")
            .map_err(|e| anyhow!("Actions module not found: {e}"))?;

        let on_action_fn: mlua::Function = actions_module
            .get("on_action")
            .map_err(|e| anyhow!("on_action function not found: {e}"))?;

        let ctx_table = self.build_action_context_table(context)?;

        let result: LuaValue = on_action_fn
            .call((action, ctx_table))
            .map_err(|e| anyhow!("Lua on_action() failed: {e}"))?;

        match result {
            LuaValue::Nil => Ok(None),
            LuaValue::Table(table) => {
                // Convert Lua table array to Vec<serde_json::Value>
                let mut ops = Vec::new();
                for pair in table.sequence_values::<LuaTable>() {
                    let op_table = pair.map_err(|e| anyhow!("Invalid op in actions result: {e}"))?;
                    let json_val = lua_table_to_json(&self.lua, &op_table)?;
                    ops.push(json_val);
                }
                Ok(Some(ops))
            }
            _ => Err(anyhow!(
                "on_action() must return a table or nil, got {:?}",
                result
            )),
        }
    }

    /// Call Lua `events.on_hub_event(event_type, event_data, context)`.
    ///
    /// Returns `Ok(Some(ops))` if Lua returned a list of compound ops,
    /// `Ok(None)` if Lua returned `nil` (event not handled).
    ///
    /// # Arguments
    ///
    /// * `event_type` - Event type string (e.g., `"agent_created"`)
    /// * `event_data` - Full event message as JSON
    /// * `context` - Current TUI state for Lua decision-making
    pub fn call_on_hub_event(
        &self,
        event_type: &str,
        event_data: &serde_json::Value,
        context: &ActionContext,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        if !self.events_loaded {
            return Ok(None);
        }

        let globals = self.lua.globals();
        let events_module: LuaTable = globals
            .get("_events")
            .map_err(|e| anyhow!("Events module not found: {e}"))?;

        let on_hub_event_fn: mlua::Function = events_module
            .get("on_hub_event")
            .map_err(|e| anyhow!("on_hub_event function not found: {e}"))?;

        // Convert event_data JSON to Lua table
        let event_table = json_to_lua_value(&self.lua, event_data)?;

        // Build context table
        let ctx_table = self.build_action_context_table(context)?;

        let result: LuaValue = on_hub_event_fn
            .call((event_type, event_table, ctx_table))
            .map_err(|e| anyhow!("Lua on_hub_event() failed: {e}"))?;

        match result {
            LuaValue::Nil => Ok(None),
            LuaValue::Table(table) => {
                let mut ops = Vec::new();
                for pair in table.sequence_values::<LuaTable>() {
                    let op_table = pair.map_err(|e| anyhow!("Invalid op in events result: {e}"))?;
                    let json_val = lua_table_to_json(&self.lua, &op_table)?;
                    ops.push(json_val);
                }
                Ok(Some(ops))
            }
            _ => Err(anyhow!(
                "on_hub_event() must return a table or nil, got {:?}",
                result
            )),
        }
    }

    /// Call Lua `handle_key(descriptor, mode, context)`.
    ///
    /// Returns `Ok(Some(action))` if Lua returned an action table,
    /// `Ok(None)` if Lua returned `nil` (unbound key — caller decides
    /// whether to forward to PTY or ignore).
    ///
    /// # Arguments
    ///
    /// * `descriptor` - Key descriptor string (e.g., `"ctrl+p"`, `"shift+enter"`)
    /// * `mode` - Current app mode as Lua string (e.g., `"normal"`, `"menu"`)
    /// * `context` - Additional context for keybinding logic
    pub fn call_handle_key(
        &self,
        descriptor: &str,
        mode: &str,
        context: &KeyContext,
    ) -> Result<Option<LuaKeyAction>> {
        if !self.keybindings_loaded {
            return Ok(None);
        }

        let globals = self.lua.globals();
        let kb_module: LuaTable = globals
            .get("_keybindings")
            .map_err(|e| anyhow!("Keybindings module not found: {e}"))?;

        let handle_key_fn: mlua::Function = kb_module
            .get("handle_key")
            .map_err(|e| anyhow!("handle_key function not found: {e}"))?;

        // Build context table
        let ctx_table = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create context table: {e}"))?;
        set_field(&ctx_table, "list_selected", context.list_selected)?;
        set_field(&ctx_table, "list_count", context.list_count)?;
        set_field(&ctx_table, "terminal_rows", context.terminal_rows)?;

        let result: LuaValue = handle_key_fn
            .call((descriptor, mode, ctx_table))
            .map_err(|e| anyhow!("Lua handle_key() failed: {e}"))?;

        match result {
            LuaValue::Nil => Ok(None),
            LuaValue::Table(table) => {
                let action: String = table
                    .get("action")
                    .map_err(|e| anyhow!("handle_key() result missing 'action': {e}"))?;

                let char_val: Option<String> = table.get("char").ok();
                let index_val: Option<usize> = table.get("index").ok();

                Ok(Some(LuaKeyAction {
                    action,
                    char: char_val.and_then(|s| s.chars().next()),
                    index: index_val,
                }))
            }
            _ => Err(anyhow!(
                "handle_key() must return a table or nil, got {:?}",
                result
            )),
        }
    }

    /// Build a Lua table from an `ActionContext`.
    ///
    /// Shared by `call_on_action()` and `call_on_hub_event()` so both
    /// Lua callbacks receive the same context shape.
    fn build_action_context_table(&self, context: &ActionContext) -> Result<LuaTable> {
        let ctx_table = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create action context table: {e}"))?;
        set_field(&ctx_table, "mode", context.mode.as_str())?;
        set_field(&ctx_table, "input_buffer", context.input_buffer.as_str())?;
        set_field(&ctx_table, "list_selected", context.list_selected)?;
        set_field(&ctx_table, "active_pty_index", context.active_pty_index)?;

        // overlay_actions array
        let actions_arr = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create overlay_actions table: {e}"))?;
        for (i, a) in context.overlay_actions.iter().enumerate() {
            actions_arr
                .set(i + 1, a.as_str())
                .map_err(|e| anyhow!("Failed to set overlay_action: {e}"))?;
        }
        ctx_table
            .set("overlay_actions", actions_arr)
            .map_err(|e| anyhow!("Failed to set overlay_actions: {e}"))?;

        // pending_fields table
        let pending = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create pending_fields table: {e}"))?;
        for (key, value) in &context.pending_fields {
            set_field(&pending, key.as_str(), value.as_str())?;
        }
        ctx_table
            .set("pending_fields", pending)
            .map_err(|e| anyhow!("Failed to set pending_fields: {e}"))?;

        // selected_agent (string or nil)
        if let Some(ref agent) = context.selected_agent {
            set_field(&ctx_table, "selected_agent", agent.as_str())?;
        }

        // selected_agent_index (0-based, or nil)
        if let Some(idx) = context.selected_agent_index {
            set_field(&ctx_table, "selected_agent_index", idx)?;
        }

        // agents array with id and session_count for navigation
        let agents_arr = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create agents table: {e}"))?;
        for (i, agent) in context.agents.iter().enumerate() {
            let a = self
                .lua
                .create_table()
                .map_err(|e| anyhow!("Failed to create agent table: {e}"))?;
            set_field(&a, "id", agent.id.as_str())?;
            set_field(&a, "session_count", agent.session_count)?;
            agents_arr
                .set(i + 1, a)
                .map_err(|e| anyhow!("Failed to set agent: {e}"))?;
        }
        ctx_table
            .set("agents", agents_arr)
            .map_err(|e| anyhow!("Failed to set agents: {e}"))?;

        // available_worktrees array of {path, branch}
        let worktrees = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create worktrees table: {e}"))?;
        for (i, (path, branch)) in context.available_worktrees.iter().enumerate() {
            let w = self
                .lua
                .create_table()
                .map_err(|e| anyhow!("Failed to create worktree table: {e}"))?;
            set_field(&w, "path", path.as_str())?;
            set_field(&w, "branch", branch.as_str())?;
            worktrees
                .set(i + 1, w)
                .map_err(|e| anyhow!("Failed to set worktree: {e}"))?;
        }
        ctx_table
            .set("available_worktrees", worktrees)
            .map_err(|e| anyhow!("Failed to set available_worktrees: {e}"))?;

        Ok(ctx_table)
    }
}

/// Serialize RenderContext into a Lua table for layout functions.
///
/// Only includes fields needed for layout decisions (not Arc<Mutex<Parser>>
/// or other non-serializable state).
fn render_context_to_lua(lua: &Lua, ctx: &RenderContext) -> Result<LuaTable> {
    let state = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create state table: {e}"))?;

    // Mode (passed directly as string — Lua owns mode names)
    set_field(&state, "mode", ctx.mode.as_str())?;

    // Agent state
    set_field(&state, "agent_count", ctx.agents.len())?;
    set_field(&state, "selected_agent_index", ctx.selected_agent_index)?;

    // Serialize agents array
    let agents_arr = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create agents table: {e}"))?;
    for (i, agent) in ctx.agents.iter().enumerate() {
        let a = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create agent table: {e}"))?;
        set_field(&a, "key", agent.key.as_str())?;
        set_field(&a, "branch_name", agent.branch_name.as_str())?;
        if let Some(ref dn) = agent.display_name {
            set_field(&a, "display_name", dn.as_str())?;
        }
        set_field(&a, "repo", agent.repo.as_str())?;
        if let Some(n) = agent.issue_number {
            set_field(&a, "issue_number", n)?;
        }
        if let Some(p) = agent.port {
            set_field(&a, "port", p)?;
        }
        set_field(&a, "server_running", agent.server_running)?;

        // Session names
        let sessions = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create sessions table: {e}"))?;
        for (j, name) in agent.session_names.iter().enumerate() {
            sessions
                .set(j + 1, name.as_str())
                .map_err(|e| anyhow!("Failed to set session name: {e}"))?;
        }
        a.set("session_names", sessions)
            .map_err(|e| anyhow!("Failed to set session_names: {e}"))?;

        agents_arr
            .set(i + 1, a)
            .map_err(|e| anyhow!("Failed to set agent: {e}"))?;
    }
    state
        .set("agents", agents_arr)
        .map_err(|e| anyhow!("Failed to set agents: {e}"))?;

    // Selected agent info (shortcut)
    if let Some(agent) = ctx.selected_agent() {
        let sa = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create selected_agent table: {e}"))?;
        set_field(&sa, "branch_name", agent.branch_name.as_str())?;
        if let Some(p) = agent.port {
            set_field(&sa, "port", p)?;
        }
        set_field(&sa, "server_running", agent.server_running)?;
        set_field(&sa, "session_count", agent.session_names.len())?;

        // Session names for terminal title computation
        let sa_sessions = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create selected_agent sessions table: {e}"))?;
        for (j, name) in agent.session_names.iter().enumerate() {
            sa_sessions
                .set(j + 1, name.as_str())
                .map_err(|e| anyhow!("Failed to set selected_agent session name: {e}"))?;
        }
        sa.set("session_names", sa_sessions)
            .map_err(|e| anyhow!("Failed to set selected_agent session_names: {e}"))?;

        state
            .set("selected_agent", sa)
            .map_err(|e| anyhow!("Failed to set selected_agent: {e}"))?;
    }

    // Terminal state
    set_field(&state, "active_pty_index", ctx.active_pty_index)?;
    set_field(&state, "scroll_offset", ctx.scroll_offset)?;
    set_field(&state, "is_scrolled", ctx.is_scrolled)?;

    // Terminal dimensions (for responsive layout calculations)
    set_field(&state, "terminal_cols", ctx.terminal_cols)?;
    set_field(&state, "terminal_rows", ctx.terminal_rows)?;

    // Status indicators
    set_field(&state, "seconds_since_poll", ctx.seconds_since_poll)?;
    set_field(&state, "poll_interval", ctx.poll_interval)?;

    let vpn_str = match ctx.vpn_status {
        Some(VpnStatus::Connected) => "connected",
        Some(VpnStatus::Connecting) => "connecting",
        Some(VpnStatus::Error) => "error",
        Some(VpnStatus::Disconnected) => "disconnected",
        None => "disabled",
    };
    set_field(&state, "vpn_status", vpn_str)?;

    // Modal-specific state
    set_field(&state, "list_selected", ctx.list_selected)?;
    set_field(&state, "input_buffer", ctx.input_buffer)?;
    set_field(&state, "bundle_used", ctx.bundle_used)?;

    // QR code dimensions for responsive connection code modal
    if let Some(cc) = ctx.connection_code {
        set_field(&state, "qr_width", cc.qr_width)?;
        set_field(&state, "qr_height", cc.qr_height)?;
    }

    if let Some(error_msg) = ctx.error_message {
        set_field(&state, "error_message", error_msg)?;
    }

    // Pending fields (generic key-value store for in-progress operations)
    let pending = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create pending_fields table: {e}"))?;
    for (key, value) in ctx.pending_fields {
        set_field(&pending, key.as_str(), value.as_str())?;
    }
    state
        .set("pending_fields", pending)
        .map_err(|e| anyhow!("Failed to set pending_fields: {e}"))?;

    // Legacy creating_agent table (for Lua layout backward compat)
    if let (Some(identifier), Some(stage)) = (
        ctx.pending_fields.get("creating_agent_id"),
        ctx.pending_fields.get("creating_agent_stage"),
    ) {
        let creating = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create creating_agent table: {e}"))?;
        set_field(&creating, "identifier", identifier.as_str())?;
        set_field(&creating, "stage", stage.as_str())?;
        state
            .set("creating_agent", creating)
            .map_err(|e| anyhow!("Failed to set creating_agent: {e}"))?;
    }
    let worktrees = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create worktrees table: {e}"))?;
    for (i, (path, branch)) in ctx.available_worktrees.iter().enumerate() {
        let w = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create worktree table: {e}"))?;
        set_field(&w, "path", path.as_str())?;
        set_field(&w, "branch", branch.as_str())?;
        worktrees
            .set(i + 1, w)
            .map_err(|e| anyhow!("Failed to set worktree: {e}"))?;
    }
    state
        .set("available_worktrees", worktrees)
        .map_err(|e| anyhow!("Failed to set available_worktrees: {e}"))?;

    Ok(state)
}

/// Helper to set a field on a Lua table with error context.
fn set_field<V: mlua::IntoLua>(table: &LuaTable, key: &str, value: V) -> Result<()> {
    table
        .set(key, value)
        .map_err(|e| anyhow!("Failed to set field '{key}': {e}"))
}

/// Convert a Lua table to a `serde_json::Value`.
///
/// Handles nested tables, strings, numbers, and booleans. Used to convert
/// compound action ops from Lua into JSON that `execute_lua_ops()` can process.
/// Convert a `serde_json::Value` to a Lua value.
///
/// Used by `call_on_hub_event()` to pass event data to Lua.
fn json_to_lua_value(lua: &Lua, value: &serde_json::Value) -> Result<LuaValue> {
    match value {
        serde_json::Value::Null => Ok(LuaValue::Nil),
        serde_json::Value::Bool(b) => Ok(LuaValue::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(LuaValue::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(LuaValue::Number(f))
            } else {
                Ok(LuaValue::Nil)
            }
        }
        serde_json::Value::String(s) => {
            let lua_str = lua
                .create_string(s.as_str())
                .map_err(|e| anyhow!("Failed to create Lua string: {e}"))?;
            Ok(LuaValue::String(lua_str))
        }
        serde_json::Value::Array(arr) => {
            let table = lua
                .create_table()
                .map_err(|e| anyhow!("Failed to create Lua table for array: {e}"))?;
            for (i, v) in arr.iter().enumerate() {
                let lua_val = json_to_lua_value(lua, v)?;
                table
                    .set(i + 1, lua_val)
                    .map_err(|e| anyhow!("Failed to set array element: {e}"))?;
            }
            Ok(LuaValue::Table(table))
        }
        serde_json::Value::Object(map) => {
            let table = lua
                .create_table()
                .map_err(|e| anyhow!("Failed to create Lua table for object: {e}"))?;
            for (k, v) in map {
                let lua_val = json_to_lua_value(lua, v)?;
                table
                    .set(k.as_str(), lua_val)
                    .map_err(|e| anyhow!("Failed to set object field '{k}': {e}"))?;
            }
            Ok(LuaValue::Table(table))
        }
    }
}

/// Convert a Lua table to a `serde_json::Value`.
///
/// Handles nested tables, strings, numbers, and booleans. Used to convert
/// compound action ops from Lua into JSON that `execute_lua_ops()` can process.
fn lua_table_to_json(lua: &Lua, table: &LuaTable) -> Result<serde_json::Value> {
    use serde_json::{Map, Value};

    let mut map = Map::new();
    for pair in table.pairs::<String, LuaValue>() {
        let (key, value) = pair.map_err(|e| anyhow!("Failed to iterate Lua table: {e}"))?;
        let json_val = lua_value_to_json(lua, &value)?;
        map.insert(key, json_val);
    }
    Ok(Value::Object(map))
}

/// Convert a single Lua value to a `serde_json::Value`.
fn lua_value_to_json(lua: &Lua, value: &LuaValue) -> Result<serde_json::Value> {
    use serde_json::Value;

    match value {
        LuaValue::Nil => Ok(Value::Null),
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        LuaValue::Integer(n) => Ok(Value::Number((*n).into())),
        LuaValue::Number(n) => {
            serde_json::Number::from_f64(*n)
                .map(Value::Number)
                .ok_or_else(|| anyhow!("Cannot convert NaN/Inf to JSON"))
        }
        LuaValue::String(s) => {
            let s = s.to_str().map_err(|e| anyhow!("Non-UTF8 Lua string: {e}"))?;
            Ok(Value::String(s.to_string()))
        }
        LuaValue::Table(t) => {
            // Check if it's an array (sequential integer keys starting at 1)
            let len = t.raw_len();
            if len > 0 {
                // Try as array first
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    let v: LuaValue = t.get(i).map_err(|e| anyhow!("Array index {i}: {e}"))?;
                    arr.push(lua_value_to_json(lua, &v)?);
                }
                Ok(Value::Array(arr))
            } else {
                // Object/map
                lua_table_to_json(lua, t)
            }
        }
        _ => Ok(Value::Null), // Functions, userdata, etc. → null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render::AgentRenderInfo;

    fn make_test_ctx() -> (Vec<AgentRenderInfo>, Vec<String>) {
        let agents = vec![AgentRenderInfo {
            key: "test-1".to_string(),
            display_name: None,
            repo: "test/repo".to_string(),
            issue_number: Some(42),
            branch_name: "feature-branch".to_string(),
            port: None,
            server_running: false,
            session_names: vec!["agent".to_string()],
        }];
        let agent_ids = vec!["test-1".to_string()];
        (agents, agent_ids)
    }

    #[test]
    fn test_layout_lua_basic_render() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return {
                    type = "hsplit",
                    constraints = { "30%", "70%" },
                    children = {
                        { type = "list" },
                        { type = "terminal" },
                    }
                }
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let tree = layout.call_render(&ctx).unwrap();
        assert!(matches!(tree, RenderNode::HSplit { .. }));
    }

    #[test]
    fn test_layout_lua_render_uses_state() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                local title = string.format(" Agents (%d) ", state.agent_count)
                return {
                    type = "hsplit",
                    constraints = { "30%", "70%" },
                    children = {
                        { type = "list", block = { title = title, borders = "all" } },
                        { type = "terminal" },
                    }
                }
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let tree = layout.call_render(&ctx).unwrap();
        match tree {
            RenderNode::HSplit { children, .. } => {
                match &children[0] {
                    RenderNode::Widget { block, .. } => {
                        let block = block.as_ref().unwrap();
                        assert_eq!(block.title.as_ref().and_then(|t| t.as_plain_str()), Some(" Agents (1) "));
                    }
                    _ => panic!("Expected Widget"),
                }
            }
            _ => panic!("Expected HSplit"),
        }
    }

    #[test]
    fn test_layout_lua_overlay_nil() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "empty" }
            end
            function render_overlay(state)
                return nil
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let overlay = layout.call_render_overlay(&ctx).unwrap();
        assert!(overlay.is_none());
    }

    #[test]
    fn test_layout_lua_overlay_menu() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "empty" }
            end
            function render_overlay(state)
                if state.mode == "menu" then
                    return {
                        type = "centered", width = 50, height = 40,
                        child = { type = "list", block = { title = " Menu ", borders = "all" } }
                    }
                end
                return nil
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "menu".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let overlay = layout.call_render_overlay(&ctx).unwrap();
        assert!(overlay.is_some());
        assert!(matches!(overlay.unwrap(), RenderNode::Centered { .. }));
    }

    /// Verifies that the actual layout.lua handles every mode string correctly.
    ///
    /// This is the key consistency test: Rust passes mode strings directly
    /// to Lua, and layout.lua must handle each one.
    #[test]
    fn test_mode_string_consistency_with_actual_layout() {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let layout = LayoutLua::new(layout_source).expect("actual layout.lua should load");

        let (agents, agent_ids) = make_test_ctx();

        // Map of every mode string to whether it should produce an overlay
        let mode_expectations: Vec<(&str, bool)> = vec![
            ("normal", false),
            ("menu", true),
            ("new_agent_select_worktree", true),
            ("new_agent_create_worktree", true),
            ("new_agent_prompt", true),
            ("close_agent_confirm", true),
            ("connection_code", true),
            ("error", true),
        ];

        for (label, expect_overlay) in &mode_expectations {
            let ctx = RenderContext {
                mode: label.to_string(),
                list_selected: 0,
                input_buffer: "",
                available_worktrees: &[],
                error_message: Some("test error"),
                pending_fields: &std::collections::HashMap::new(),
                connection_code: None,
                bundle_used: false,
                agent_ids: &agent_ids,
                agents: &agents,
                selected_agent_index: 0,
                active_parser: None,
                parser_pool: &std::collections::HashMap::new(),
                active_pty_index: 0,
                scroll_offset: 0,
                is_scrolled: false,
                seconds_since_poll: 0,
                poll_interval: 10,
                vpn_status: None,
                terminal_cols: 80,
                terminal_rows: 24,
                terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
            };

            // Main render should always succeed
            let tree = layout.call_render(&ctx);
            assert!(tree.is_ok(), "render() failed for mode '{label}': {:?}", tree.err());

            // Overlay should match expectation
            let overlay = layout.call_render_overlay(&ctx);
            assert!(overlay.is_ok(), "render_overlay() failed for mode '{label}': {:?}", overlay.err());

            let has_overlay = overlay.unwrap().is_some();
            assert_eq!(
                has_overlay, *expect_overlay,
                "Mode '{label}': expected overlay={expect_overlay}, got overlay={has_overlay}"
            );
        }
    }

    /// Verifies that overlay nodes from the actual layout.lua parse into
    /// valid Centered → Widget trees with correct widget types.
    #[test]
    fn test_actual_layout_overlay_widget_types() {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let layout = LayoutLua::new(layout_source).expect("actual layout.lua should load");

        let (agents, agent_ids) = make_test_ctx();

        // Each modal mode and its expected inner widget type (as Debug string contains)
        let mode_widgets: Vec<(&str, &str)> = vec![
            ("menu", "List"),
            ("new_agent_select_worktree", "List"),
            ("new_agent_create_worktree", "Input"),
            ("new_agent_prompt", "Input"),
            ("close_agent_confirm", "Paragraph"),
            ("connection_code", "ConnectionCode"),
            ("error", "Paragraph"),
        ];

        for (mode, expected_widget) in &mode_widgets {
            let ctx = RenderContext {
                mode: mode.to_string(),
                list_selected: 0,
                input_buffer: "",
                available_worktrees: &[],
                error_message: Some("test"),
                pending_fields: &std::collections::HashMap::new(),
                connection_code: None,
                bundle_used: false,
                agent_ids: &agent_ids,
                agents: &agents,
                selected_agent_index: 0,
                active_parser: None,
                parser_pool: &std::collections::HashMap::new(),
                active_pty_index: 0,
                scroll_offset: 0,
                is_scrolled: false,
                seconds_since_poll: 0,
                poll_interval: 10,
                vpn_status: None,
                terminal_cols: 80,
                terminal_rows: 24,
                terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
            };

            let overlay = layout.call_render_overlay(&ctx).unwrap().unwrap();
            match overlay {
                RenderNode::Centered { child, .. } => {
                    let dbg = format!("{child:?}");
                    assert!(
                        dbg.contains(expected_widget),
                        "Mode {:?}: expected widget containing '{expected_widget}', got: {dbg}",
                        mode
                    );
                }
                _ => panic!("Mode {:?}: expected Centered overlay, got {:?}", mode, overlay),
            }
        }
    }

    #[test]
    fn test_layout_lua_error_fallback() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                error("intentional error")
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let result = layout.call_render(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_layout_lua_reload() {
        let layout = LayoutLua::new(
            r#"
            function render(state) return { type = "empty" } end
        "#,
        )
        .unwrap();

        // Reload with new layout
        layout
            .reload(
                r#"
            function render(state)
                return {
                    type = "hsplit",
                    constraints = { "50%", "50%" },
                    children = {
                        { type = "list" },
                        { type = "terminal" },
                    }
                }
            end
        "#,
            )
            .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let tree = layout.call_render(&ctx).unwrap();
        assert!(matches!(tree, RenderNode::HSplit { .. }));
    }

    #[test]
    fn test_load_extension_wraps_render() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "terminal" }
            end
        "#,
        )
        .unwrap();

        // Extension wraps render to add a vsplit wrapper
        layout
            .load_extension(
                r#"
            local _original = render
            function render(state)
                return {
                    type = "vsplit",
                    constraints = { "95%", "5%" },
                    children = {
                        _original(state),
                        { type = "paragraph", props = { lines = { "status bar" } } },
                    }
                }
            end
        "#,
                "test_extension",
            )
            .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let tree = layout.call_render(&ctx).unwrap();
        // Extension wrapped the terminal in a vsplit
        assert!(matches!(tree, RenderNode::VSplit { .. }));
    }

    #[test]
    fn test_load_extension_error_does_not_break_state() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "terminal" }
            end
        "#,
        )
        .unwrap();

        // Bad extension with syntax error
        let result = layout.load_extension("function broken(", "bad_extension");
        assert!(result.is_err());

        // Original render still works
        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        let tree = layout.call_render(&ctx).unwrap();
        assert!(matches!(tree, RenderNode::Widget { .. }));
    }

    #[test]
    fn test_load_extension_multiple_chain() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "terminal" }
            end
            function render_overlay(state)
                return nil
            end
        "#,
        )
        .unwrap();

        // First extension: adds a global
        layout
            .load_extension(
                r#"
            _test_value = 42
        "#,
                "ext1",
            )
            .unwrap();

        // Second extension: uses the global from first
        layout
            .load_extension(
                r#"
            local _original = render
            function render(state)
                if _test_value == 42 then
                    return {
                        type = "hsplit",
                        constraints = { "50%", "50%" },
                        children = {
                            _original(state),
                            { type = "empty" },
                        }
                    }
                end
                return _original(state)
            end
        "#,
                "ext2",
            )
            .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: "normal".to_string(),
            list_selected: 0,
            input_buffer: "",
            available_worktrees: &[],
            error_message: None,
            pending_fields: &std::collections::HashMap::new(),
            connection_code: None,
            bundle_used: false,
            agent_ids: &agent_ids,
            agents: &agents,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: &std::collections::HashMap::new(),
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        // Second extension saw _test_value from first, so wrapped in hsplit
        let tree = layout.call_render(&ctx).unwrap();
        assert!(matches!(tree, RenderNode::HSplit { .. }));
    }

    #[test]
    fn test_botster_api_loads_and_provides_globals() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "terminal" }
            end
        "#,
        )
        .unwrap();

        // Load botster API
        let botster_source = include_str!("../../lua/ui/botster.lua");
        layout
            .load_extension(botster_source, "botster")
            .expect("botster.lua should load without errors");

        // Verify botster global exists and has expected API
        layout
            .load_extension(
                r#"
            assert(type(botster) == "table", "botster global should exist")
            assert(type(botster.keymap) == "table", "botster.keymap should exist")
            assert(type(botster.keymap.set) == "function", "botster.keymap.set should exist")
            assert(type(botster.keymap.del) == "function", "botster.keymap.del should exist")
            assert(type(botster.keymap.list) == "function", "botster.keymap.list should exist")
            assert(type(botster.action) == "table", "botster.action should exist")
            assert(type(botster.action.register) == "function", "botster.action.register should exist")
            assert(type(botster.ui) == "table", "botster.ui should exist")
            assert(type(botster.ui.register_component) == "function", "register_component should exist")
            assert(type(botster.tbl_deep_extend) == "function", "tbl_deep_extend should exist")
            assert(type(botster.g) == "table", "botster.g should exist")
        "#,
                "verify_botster",
            )
            .expect("botster API should have all expected functions");
    }

    /// Create a LayoutLua with layout + keybindings + actions + botster API loaded.
    /// Mimics the full production init chain.
    fn make_full_lua() -> LayoutLua {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let botster_source = include_str!("../../lua/ui/botster.lua");

        let mut lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        lua.load_keybindings(kb_source).expect("keybindings.lua should load");
        lua.load_actions(actions_source).expect("actions.lua should load");
        lua.load_extension(botster_source, "botster").expect("botster.lua should load");
        lua
    }

    #[test]
    fn test_botster_keymap_set_string_action() {
        let lua = make_full_lua();

        // Register a new keybinding via botster API
        lua.load_extension(r#"
            botster.keymap.set("normal", "ctrl+n", "open_menu", {
                desc = "Quick new agent",
                namespace = "test",
            })
        "#, "test_keymap").unwrap();

        // Wire dispatch
        lua.load_extension(
            "botster._wire_keybindings()",
            "_wire",
        ).unwrap();

        // Verify the keybinding works
        let ctx = KeyContext { list_selected: 0, list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+n", "normal", &ctx).unwrap();
        assert!(result.is_some(), "ctrl+n should be bound");
        assert_eq!(result.unwrap().action, "open_menu");
    }

    #[test]
    fn test_botster_keymap_set_function_action() {
        let lua = make_full_lua();

        // Register a function-based keybinding
        lua.load_extension(r#"
            botster.keymap.set("normal", "ctrl+n", function(context)
                return { action = "toggle_pty" }
            end, { desc = "Smart toggle" })
        "#, "test_fn_keymap").unwrap();

        // Wire dispatch
        lua.load_extension("botster._wire_keybindings()", "_wire").unwrap();

        // Verify function-based keybinding resolves
        let ctx = KeyContext { list_selected: 0, list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+n", "normal", &ctx).unwrap();
        assert!(result.is_some(), "ctrl+n function binding should resolve");
        assert_eq!(result.unwrap().action, "toggle_pty");
    }

    #[test]
    fn test_botster_keymap_del() {
        let lua = make_full_lua();

        // ctrl+p is bound to open_menu in built-in keybindings
        let ctx = KeyContext { list_selected: 0, list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+p", "normal", &ctx).unwrap();
        assert!(result.is_some(), "ctrl+p should be bound initially");

        // Delete the binding
        lua.load_extension(r#"
            botster.keymap.del("normal", "ctrl+p")
        "#, "test_del").unwrap();

        lua.load_extension("botster._wire_keybindings()", "_wire").unwrap();

        let result = lua.call_handle_key("ctrl+p", "normal", &ctx).unwrap();
        assert!(result.is_none(), "ctrl+p should be unbound after del");
    }

    #[test]
    fn test_botster_keymap_clear_namespace() {
        let lua = make_full_lua();

        // Register two bindings under same namespace
        lua.load_extension(r#"
            botster.keymap.set("normal", "ctrl+n", "action_a", { namespace = "myplugin" })
            botster.keymap.set("normal", "ctrl+m", "action_b", { namespace = "myplugin" })
        "#, "test_ns").unwrap();

        lua.load_extension("botster._wire_keybindings()", "_wire").unwrap();

        let ctx = KeyContext { list_selected: 0, list_count: 0, terminal_rows: 24 };

        // Both should be bound
        assert!(lua.call_handle_key("ctrl+n", "normal", &ctx).unwrap().is_some());
        assert!(lua.call_handle_key("ctrl+m", "normal", &ctx).unwrap().is_some());

        // Clear the namespace
        lua.load_extension(r#"
            botster.keymap.clear_namespace("myplugin")
        "#, "test_clear").unwrap();

        lua.load_extension("botster._wire_keybindings()", "_wire").unwrap();

        // Both should be unbound
        assert!(lua.call_handle_key("ctrl+n", "normal", &ctx).unwrap().is_none());
        assert!(lua.call_handle_key("ctrl+m", "normal", &ctx).unwrap().is_none());
    }

    #[test]
    fn test_botster_action_register_and_dispatch() {
        let lua = make_full_lua();

        // Register a custom action
        lua.load_extension(r#"
            botster.action.register("my_custom_action", function(context)
                return {
                    { op = "set_mode", mode = "menu" },
                }
            end, { desc = "Test action" })
        "#, "test_action").unwrap();

        // Wire dispatch
        lua.load_extension("botster._wire_actions()", "_wire").unwrap();

        // Dispatch the custom action
        let ctx = ActionContext {
            mode: "normal".to_string(),
            ..Default::default()
        };
        let result = lua.call_on_action("my_custom_action", &ctx).unwrap();
        assert!(result.is_some(), "Custom action should dispatch");
        let ops = result.unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "menu");
    }

    #[test]
    fn test_botster_action_falls_through_to_builtin() {
        let lua = make_full_lua();

        // Wire dispatch (no custom actions registered)
        lua.load_extension("botster._wire_actions()", "_wire").unwrap();

        // Built-in "open_menu" should still work
        let ctx = ActionContext {
            mode: "normal".to_string(),
            ..Default::default()
        };
        let result = lua.call_on_action("open_menu", &ctx).unwrap();
        assert!(result.is_some(), "Built-in action should still dispatch");
        let ops = result.unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "menu");
    }

    #[test]
    fn test_botster_keymap_list() {
        let lua = make_full_lua();

        lua.load_extension(r#"
            botster.keymap.set("normal", "ctrl+n", "my_action", {
                desc = "My description",
                namespace = "test_ns",
            })
            local bindings = botster.keymap.list({ namespace = "test_ns" })
            assert(#bindings == 1, "Should have 1 binding in test_ns")
            assert(bindings[1].key == "ctrl+n", "Key should be ctrl+n")
            assert(bindings[1].desc == "My description", "Desc should match")
            assert(bindings[1].namespace == "test_ns", "Namespace should match")
        "#, "test_list").unwrap();
    }

    #[test]
    fn test_botster_g_persists_across_reload() {
        let lua = make_full_lua();

        // Set state in botster.g
        lua.load_extension(r#"
            botster.g.counter = 42
        "#, "set_state").unwrap();

        // Reload botster.lua (simulates hot-reload)
        let botster_source = include_str!("../../lua/ui/botster.lua");
        lua.load_extension(botster_source, "botster_reload").unwrap();

        // Verify state persisted
        lua.load_extension(r#"
            assert(botster.g.counter == 42, "botster.g should persist across reload, got: " .. tostring(botster.g.counter))
        "#, "verify_state").unwrap();
    }

    #[test]
    fn test_botster_wire_idempotent_no_recursive_wrapping() {
        let lua = make_full_lua();

        // Register a custom action
        lua.load_extension(r#"
            botster.action.register("test_action", function(context)
                return { { op = "set_mode", mode = "menu" } }
            end)
        "#, "register").unwrap();

        // Wire TWICE (simulates hot-reload calling wire again)
        lua.load_extension("botster._wire_actions() botster._wire_keybindings()", "_wire1").unwrap();
        lua.load_extension("botster._wire_actions() botster._wire_keybindings()", "_wire2").unwrap();

        // Custom action should still work (not infinite recursion)
        let ctx = ActionContext {
            mode: "normal".to_string(),
            ..Default::default()
        };
        let result = lua.call_on_action("test_action", &ctx).unwrap();
        assert!(result.is_some(), "Custom action should work after double-wire");

        // Built-in action should also still work (fallthrough intact)
        let result2 = lua.call_on_action("open_menu", &ctx).unwrap();
        assert!(result2.is_some(), "Built-in action should work after double-wire");
    }

    #[test]
    fn test_botster_tbl_deep_extend() {
        let layout = LayoutLua::new(
            r#"
            function render(state)
                return { type = "terminal" }
            end
        "#,
        )
        .unwrap();

        let botster_source = include_str!("../../lua/ui/botster.lua");
        layout.load_extension(botster_source, "botster").unwrap();

        layout
            .load_extension(
                r#"
            -- Force behavior: later values win
            local result = botster.tbl_deep_extend("force",
                { a = 1, b = { x = 10, y = 20 } },
                { a = 2, b = { y = 30, z = 40 } }
            )
            assert(result.a == 2, "force: scalar should be overwritten")
            assert(result.b.x == 10, "force: nested key should be preserved")
            assert(result.b.y == 30, "force: nested key should be overwritten")
            assert(result.b.z == 40, "force: new nested key should appear")

            -- Keep behavior: earlier values win
            local result2 = botster.tbl_deep_extend("keep",
                { a = 1, b = 2 },
                { a = 99, c = 3 }
            )
            assert(result2.a == 1, "keep: existing scalar should be preserved")
            assert(result2.b == 2, "keep: existing key preserved")
            assert(result2.c == 3, "keep: new key should appear")
        "#,
                "test_merge",
            )
            .expect("tbl_deep_extend should work correctly");
    }
}
