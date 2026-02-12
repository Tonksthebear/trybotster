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
        })
    }

    /// Reload the layout from new source (for hot-reload).
    pub fn reload(&self, lua_source: &str) -> Result<()> {
        self.lua
            .load(lua_source)
            .exec()
            .map_err(|e| anyhow!("Failed to reload layout Lua: {e}"))
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

        // Build context table
        let ctx_table = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create action context table: {e}"))?;
        set_field(&ctx_table, "mode", context.mode.as_str())?;
        set_field(&ctx_table, "input_buffer", context.input_buffer.as_str())?;
        set_field(&ctx_table, "list_selected", context.list_selected)?;

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
}
