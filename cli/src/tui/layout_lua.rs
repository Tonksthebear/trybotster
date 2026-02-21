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
/// Contains only Rust-owned state that keybinding fallback logic needs.
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    /// Total number of selectable list items (from Rust's overlay_list_actions).
    pub list_count: usize,
    /// Terminal height in rows (for scroll amount calculation).
    pub terminal_rows: u16,
}

/// Context passed to Lua `actions.on_action()` for workflow dispatch.
///
/// Contains only Rust-owned state that Lua needs. Application state
/// (mode, input_buffer, list_selected, agents, pending_fields, worktrees)
/// lives in Lua's `_tui_state` global — the TUI is a client like the browser.
#[derive(Debug, Clone, Default)]
pub struct ActionContext {
    /// Action strings from the overlay list (extracted from render tree).
    pub overlay_actions: Vec<String>,
    /// Currently selected agent ID (if any).
    pub selected_agent: Option<String>,
    /// Character for `input_char` action (set by Rust when dispatching).
    pub action_char: Option<char>,
    /// Whether the outer terminal window has OS-level focus.
    pub terminal_focused: bool,
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
    /// Execute a Lua snippet. Used for setting `_tui_state` fields.
    pub fn exec(&self, source: &str) -> Result<()> {
        self.lua
            .load(source)
            .exec()
            .map_err(|e| anyhow!("Lua exec failed: {e}"))
    }

    /// Evaluate a Lua expression and return the result as a string.
    pub fn eval_string(&self, expr: &str) -> Result<String> {
        self.lua
            .load(expr)
            .eval::<String>()
            .map_err(|e| anyhow!("Lua eval failed: {e}"))
    }

    /// Evaluate a Lua expression and return the result as a usize.
    pub fn eval_usize(&self, expr: &str) -> Result<usize> {
        self.lua
            .load(expr)
            .eval::<usize>()
            .map_err(|e| anyhow!("Lua eval failed: {e}"))
    }

    /// Load a Lua UI extension by evaluating its source.
    ///
    /// # Errors
    ///
    /// Returns an error if the Lua source fails to parse or execute.
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
    /// * `context` - Action context (overlay actions, selected agent/PTY, etc.)
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
    /// Build a Lua table from an `ActionContext`.
    ///
    /// Only serializes Rust-owned state. Application state (agents,
    /// pending_fields, worktrees) is accessed directly from `_tui_state`
    /// by the Lua callbacks.
    fn build_action_context_table(&self, context: &ActionContext) -> Result<LuaTable> {
        let ctx_table = self
            .lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create action context table: {e}"))?;
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

        // selected_agent (string or nil)
        if let Some(ref agent) = context.selected_agent {
            set_field(&ctx_table, "selected_agent", agent.as_str())?;
        }

        // _char for input_char action (optional)
        if let Some(c) = context.action_char {
            set_field(&ctx_table, "_char", c.to_string().as_str())?;
        }

        ctx_table
            .set("terminal_focused", context.terminal_focused)
            .map_err(|e| anyhow!("Failed to set terminal_focused: {e}"))?;

        Ok(ctx_table)
    }
}

/// Serialize RenderContext into a Lua table for layout functions.
///
/// Only includes Rust-owned rendering state. Application state (agents,
/// pending_fields, worktrees) lives in Lua's `_tui_state` global and is
/// accessed directly by the layout functions.
fn render_context_to_lua(lua: &Lua, ctx: &RenderContext) -> Result<LuaTable> {
    let state = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create state table: {e}"))?;

    // Terminal state (selection lives in Lua's _tui_state)
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

    // Modal-specific state (list_selected, input_buffer live in Lua's _tui_state)
    set_field(&state, "bundle_used", ctx.bundle_used)?;

    // QR code dimensions for responsive connection code modal
    if let Some(cc) = ctx.connection_code {
        set_field(&state, "qr_width", cc.qr_width)?;
        set_field(&state, "qr_height", cc.qr_height)?;
    }

    if let Some(error_msg) = ctx.error_message {
        set_field(&state, "error_message", error_msg)?;
    }

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

    fn make_test_ctx(_mode: &str) -> RenderContext<'static> {
        let panels: &'static std::collections::HashMap<(usize, usize), crate::tui::terminal_panel::TerminalPanel> =
            Box::leak(Box::new(std::collections::HashMap::new()));
        RenderContext {
            error_message: None,
            connection_code: None,
            bundle_used: false,
            panels,
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
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

        let ctx = make_test_ctx("normal");
        let tree = layout.call_render(&ctx).unwrap();
        assert!(matches!(tree, RenderNode::HSplit { .. }));
    }

    #[test]
    fn test_layout_lua_render_uses_state() {
        let layout = LayoutLua::new(
            r#"
            _tui_state = { agents = { { id = "test-1", branch_name = "feature" } }, pending_fields = {} }
            function render(state)
                local agents = _tui_state and _tui_state.agents or {}
                local title = string.format(" Agents (%d) ", #agents)
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

        let ctx = make_test_ctx("normal");
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

        let ctx = make_test_ctx("normal");
        let overlay = layout.call_render_overlay(&ctx).unwrap();
        assert!(overlay.is_none());
    }

    #[test]
    fn test_layout_lua_overlay_menu() {
        let layout = LayoutLua::new(
            r#"
            _tui_state = _tui_state or { mode = "normal" }
            function render(state)
                return { type = "empty" }
            end
            function render_overlay(state)
                if _tui_state.mode == "menu" then
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

        layout.exec("_tui_state.mode = 'menu'").unwrap();
        let ctx = make_test_ctx("menu");
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

        // Bootstrap _tui_state (layout.lua reads from it)
        layout.load_extension(
            "_tui_state = { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).unwrap();

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
            layout.exec(&format!("_tui_state.mode = '{label}'")).unwrap();
            let mut ctx = make_test_ctx(label);
            ctx.error_message = Some("test error");

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

        // Bootstrap _tui_state (layout.lua reads from it)
        layout.load_extension(
            "_tui_state = { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).unwrap();

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
            layout.exec(&format!("_tui_state.mode = '{mode}'")).unwrap();
            let mut ctx = make_test_ctx(mode);
            ctx.error_message = Some("test");

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

        let ctx = make_test_ctx("normal");

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

        let ctx = make_test_ctx("normal");

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

        let ctx = make_test_ctx("normal");

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
        let ctx = make_test_ctx("normal");

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

        let ctx = make_test_ctx("normal");

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
        let events_source = include_str!("../../lua/ui/events.lua");
        let botster_source = include_str!("../../lua/ui/botster.lua");

        let mut lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        // Bootstrap _tui_state (layout.lua and actions.lua read from it)
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0, selected_agent_index = nil, active_pty_index = 0 }",
            "_tui_state_init",
        ).expect("_tui_state bootstrap should succeed");
        lua.load_keybindings(kb_source).expect("keybindings.lua should load");
        lua.load_actions(actions_source).expect("actions.lua should load");
        lua.load_events(events_source).expect("events.lua should load");
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
        let ctx = KeyContext { list_count: 0, terminal_rows: 24 };
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
        let ctx = KeyContext { list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+n", "normal", &ctx).unwrap();
        assert!(result.is_some(), "ctrl+n function binding should resolve");
        assert_eq!(result.unwrap().action, "toggle_pty");
    }

    #[test]
    fn test_botster_keymap_del() {
        let lua = make_full_lua();

        // ctrl+p is bound to open_menu in built-in keybindings
        let ctx = KeyContext { list_count: 0, terminal_rows: 24 };
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

        let ctx = KeyContext { list_count: 0, terminal_rows: 24 };

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
        let ctx = ActionContext::default();
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
        let ctx = ActionContext::default();
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
        let ctx = ActionContext::default();
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

    /// Full TUI boot sequence: layout → keybindings → actions → events → bootstrap → render.
    /// Verifies the complete init chain produces a renderable layout.
    #[test]
    fn test_full_tui_boot_sequence() {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let events_source = include_str!("../../lua/ui/events.lua");
        let botster_source = include_str!("../../lua/ui/botster.lua");

        // Mirror the exact init order from runner.rs run()
        let mut lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        lua.load_keybindings(kb_source).expect("keybindings.lua should load");
        lua.load_actions(actions_source).expect("actions.lua should load");
        lua.load_events(events_source).expect("events.lua should load");
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).expect("bootstrap should succeed");
        lua.load_extension(botster_source, "botster").expect("botster.lua should load");

        // Render should succeed in normal mode
        let ctx = make_test_ctx("normal");
        let tree = lua.call_render(&ctx);
        assert!(tree.is_ok(), "render() should succeed after full boot: {:?}", tree.err());

        // Overlay should return None in normal mode
        let overlay = lua.call_render_overlay(&ctx);
        assert!(overlay.is_ok(), "render_overlay() should succeed");
        assert!(overlay.unwrap().is_none(), "normal mode should have no overlay");

        // Key handling should work
        let key_ctx = KeyContext { list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+p", "normal", &key_ctx);
        assert!(result.is_ok(), "handle_key should work: {:?}", result.err());
    }

    /// User override extension only replaces the function it redefines.
    #[test]
    fn test_user_override_layers_on_built_in() {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).unwrap();

        // Verify built-in render works
        let ctx = make_test_ctx("normal");
        let tree = lua.call_render(&ctx);
        assert!(tree.is_ok(), "built-in render should work");

        // User override: only redefine render_overlay, leave render() untouched
        lua.load_extension(
            r#"
            function render_overlay(state)
                if _tui_state.mode == "custom_mode" then
                    return {
                        type = "centered", width = 50, height = 30,
                        child = { type = "paragraph", block = { title = " Custom! " } }
                    }
                end
                return nil
            end
            "#,
            "user_layout_override",
        ).expect("user override should load");

        // Built-in render() still works (wasn't replaced)
        let tree = lua.call_render(&ctx);
        assert!(tree.is_ok(), "render() should still work after overlay override");

        // Custom overlay activates for custom mode
        lua.exec("_tui_state.mode = 'custom_mode'").unwrap();
        let overlay = lua.call_render_overlay(&ctx).unwrap();
        assert!(overlay.is_some(), "custom overlay should appear for custom_mode");

        // Built-in overlays still work (menu mode)
        lua.exec("_tui_state.mode = 'menu'").unwrap();
        let overlay = lua.call_render_overlay(&ctx).unwrap();
        // Will be None because our override replaced render_overlay entirely —
        // the user needs to include the built-in logic too, or call the original.
        // This is expected: full function replacement, not merging.
        assert!(overlay.is_none(), "override replaced entire render_overlay");
    }

    /// User can add keybindings via botster.keymap without replacing built-in ones.
    #[test]
    fn test_user_keybinding_extension() {
        let lua = make_full_lua();

        // Built-in: ctrl+p should map to "menu" action in normal mode
        let key_ctx = KeyContext { list_count: 0, terminal_rows: 24 };
        let result = lua.call_handle_key("ctrl+p", "normal", &key_ctx).unwrap();
        assert!(result.is_some(), "built-in ctrl+p should be bound");

        // User extension adds a new binding
        lua.load_extension(
            r#"botster.keymap.set("normal", "ctrl+t", "my_custom_action", { desc = "Test" })"#,
            "user_keys",
        ).expect("user keybinding extension should load");

        // New binding works
        let result = lua.call_handle_key("ctrl+t", "normal", &key_ctx).unwrap();
        assert!(result.is_some(), "user ctrl+t should be bound");

        // Built-in binding still works
        let result = lua.call_handle_key("ctrl+p", "normal", &key_ctx).unwrap();
        assert!(result.is_some(), "built-in ctrl+p should still work");
    }

    /// User can register custom actions via botster.action.
    #[test]
    fn test_user_action_extension() {
        let lua = make_full_lua();

        // Register a custom action that returns ops
        lua.load_extension(
            r#"
            botster.action.register("my_action", function(ctx)
                return { { op = "set_mode", mode = "custom" } }
            end)
            "#,
            "user_actions",
        ).expect("user action extension should load");

        // Wire botster actions into _actions dispatch (runner.rs does this after extensions)
        lua.exec("botster._wire_actions() botster._wire_keybindings()").unwrap();

        // Dispatch the custom action
        let action_ctx = ActionContext::default();
        let result = lua.call_on_action("my_action", &action_ctx).unwrap();
        assert!(result.is_some(), "custom action should return ops");
    }

    // ========================================================================
    // Create Agent Event Flow Tests
    //
    // These tests prove the VISUAL feedback chain: hub events arrive,
    // update _tui_state, and the layout renders the correct output.
    // This is the path that makes agent creation visible to the user.
    // ========================================================================

    /// Helper: create full Lua env with events module loaded.
    fn make_full_lua_with_events() -> LayoutLua {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let events_source = include_str!("../../lua/ui/events.lua");
        let botster_source = include_str!("../../lua/ui/botster.lua");

        let mut lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_profiles = {}, mode = 'normal', input_buffer = '', list_selected = 0, selected_agent_index = nil, active_pty_index = 0 }",
            "_tui_state_init",
        ).expect("_tui_state bootstrap should succeed");
        lua.load_keybindings(kb_source).expect("keybindings.lua should load");
        lua.load_actions(actions_source).expect("actions.lua should load");
        lua.load_events(events_source).expect("events.lua should load");
        lua.load_extension(botster_source, "botster").expect("botster.lua should load");
        lua
    }

    /// After user submits create_agent, lifecycle events should update
    /// _tui_state.pending_fields to show creation progress.
    #[test]
    fn test_lifecycle_creating_worktree_updates_pending_fields() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // Simulate the hub broadcasting agent_status_changed: creating_worktree
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "my-repo-feature-auth",
            "status": "creating_worktree",
        });
        let ops = lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();
        assert!(ops.is_some(), "agent_status_changed should return ops");

        // Verify _tui_state was updated for the creating indicator
        let creating_id = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_id or 'NIL'"
        ).unwrap();
        assert_eq!(creating_id, "my-repo-feature-auth",
            "creating_agent_id should be set after creating_worktree event");

        let stage = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_stage or 'NIL'"
        ).unwrap();
        assert_eq!(stage, "creating_worktree",
            "creating_agent_stage should be 'creating_worktree'");
    }

    /// spawning_ptys lifecycle event should update stage to spawning_agent.
    #[test]
    fn test_lifecycle_spawning_ptys_updates_stage() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // First: creating_worktree
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "my-repo-42",
            "status": "creating_worktree",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();

        // Then: spawning_ptys
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "my-repo-42",
            "status": "spawning_ptys",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();

        let stage = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_stage or 'NIL'"
        ).unwrap();
        assert_eq!(stage, "spawning_agent",
            "spawning_ptys should map to 'spawning_agent' stage for display");
    }

    /// agent_created event should add agent to _tui_state.agents and clear
    /// pending_fields, making the agent visible in the sidebar.
    #[test]
    fn test_agent_created_event_adds_agent_to_state() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // Pre-condition: pending creation in progress
        lua.exec(r#"
            _tui_state.pending_fields.creating_agent_id = "my-repo-feature-auth"
            _tui_state.pending_fields.creating_agent_stage = "spawning_agent"
        "#).unwrap();

        // Simulate agent_created event from hub
        let event = serde_json::json!({
            "type": "agent_created",
            "agent": {
                "id": "my-repo-feature-auth",
                "display_name": "feature-auth",
                "repo": "my/repo",
                "branch_name": "feature-auth",
                "worktree_path": "/tmp/worktrees/feature-auth",
                "status": "running",
                "sessions": [
                    { "name": "agent", "port_forward": false },
                ],
                "created_at": 1707833400,
            }
        });
        let ops = lua.call_on_hub_event("agent_created", &event, &ctx).unwrap();
        assert!(ops.is_some(), "agent_created should return ops");

        // Verify agent was added to _tui_state.agents
        let agent_count = lua.eval_usize("return #_tui_state.agents").unwrap();
        assert_eq!(agent_count, 1, "Should have 1 agent after agent_created event");

        let agent_id = lua.eval_string("return _tui_state.agents[1].id").unwrap();
        assert_eq!(agent_id, "my-repo-feature-auth");

        let branch = lua.eval_string("return _tui_state.agents[1].branch_name").unwrap();
        assert_eq!(branch, "feature-auth");

        // Verify pending_fields were cleared
        let creating_id = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_id and 'SET' or 'NIL'"
        ).unwrap();
        assert_eq!(creating_id, "NIL",
            "creating_agent_id should be cleared after agent_created");

        let stage = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_stage and 'SET' or 'NIL'"
        ).unwrap();
        assert_eq!(stage, "NIL",
            "creating_agent_stage should be cleared after agent_created");

        // Verify ops include focus_terminal (auto-select new agent)
        let ops = ops.unwrap();
        let focus_op = ops.iter().find(|op| op["op"] == "focus_terminal");
        assert!(focus_op.is_some(),
            "agent_created should return focus_terminal op to show the agent");
        assert_eq!(focus_op.unwrap()["agent_id"], "my-repo-feature-auth");

        // Verify mode switches to insert
        let mode_op = ops.iter().find(|op| op["op"] == "set_mode");
        assert!(mode_op.is_some(), "Should switch to insert mode");
        assert_eq!(mode_op.unwrap()["mode"], "insert");
    }

    /// Extract list item plain-text strings from the sidebar (first child of HSplit).
    fn extract_sidebar_items(tree: &RenderNode) -> Vec<String> {
        use crate::tui::render_tree::{WidgetProps, ListProps, StyledContent};
        match tree {
            RenderNode::HSplit { children, .. } => {
                match &children[0] {
                    RenderNode::Widget { props, .. } => {
                        if let Some(WidgetProps::List(ListProps { items, .. })) = props {
                            items.iter().map(|item| {
                                match &item.content {
                                    StyledContent::Plain(s) => s.clone(),
                                    StyledContent::Styled(spans) => {
                                        spans.iter().map(|s| s.text.clone()).collect::<String>()
                                    }
                                }
                            }).collect()
                        } else {
                            panic!("Expected List props on sidebar widget");
                        }
                    }
                    _ => panic!("Expected Widget (list) as first child"),
                }
            }
            _ => panic!("Expected HSplit root layout"),
        }
    }

    /// Layout should render the creating indicator during creation.
    #[test]
    fn test_layout_renders_creating_indicator() {
        let lua = make_full_lua_with_events();

        lua.exec(r#"
            _tui_state.pending_fields.creating_agent_id = "feature-auth"
            _tui_state.pending_fields.creating_agent_stage = "creating_worktree"
        "#).unwrap();

        let ctx = make_test_ctx("normal");
        let tree = lua.call_render(&ctx).unwrap();
        let items = extract_sidebar_items(&tree);

        assert!(!items.is_empty(), "Should have at least the creating indicator");
        assert!(items[0].contains("feature-auth"),
            "Creating indicator should show agent identifier, got: {}", items[0]);
        assert!(items[0].contains("Creating worktree"),
            "Creating indicator should show stage, got: {}", items[0]);
    }

    /// Layout should render the agent in the sidebar after creation completes.
    #[test]
    fn test_layout_renders_created_agent_in_sidebar() {
        let lua = make_full_lua_with_events();

        lua.exec(r#"
            _tui_state.agents = {
                {
                    id = "my-repo-feature-auth",
                    display_name = "feature-auth",
                    branch_name = "feature-auth",
                    status = "running",
                    sessions = { { name = "agent" } },
                }
            }
            _tui_state.selected_agent_index = 0
        "#).unwrap();

        let ctx = make_test_ctx("insert");
        let tree = lua.call_render(&ctx).unwrap();
        let items = extract_sidebar_items(&tree);

        assert_eq!(items.len(), 1, "Should have exactly 1 agent item");
        assert_eq!(items[0], "feature-auth", "Agent item should show display name");
    }

    /// Full end-to-end: user creates agent → lifecycle events → agent appears.
    /// This is the complete visual flow test.
    #[test]
    fn test_full_visual_flow_create_agent_on_main() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // Step 1: User triggers create_agent (main branch flow)
        lua.call_on_action("open_menu", &ctx).unwrap();
        let menu_ctx = ActionContext {
            overlay_actions: vec!["new_agent".to_string()],
            ..Default::default()
        };
        lua.call_on_action("list_select", &menu_ctx).unwrap(); // → profile select
        // Simulate single-profile response (auto-skips to worktree)
        let profiles_event = serde_json::json!({ "profiles": ["claude"] });
        lua.call_on_hub_event("profiles", &profiles_event, &ctx).unwrap();
        lua.call_on_action("list_select", &ctx).unwrap(); // → Use Main Branch
        lua.exec("_tui_state.input_buffer = 'Fix bug'").unwrap();
        lua.call_on_action("input_submit", &ctx).unwrap(); // → sends create_agent

        // Verify creating indicator is set
        let creating_id = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_id or 'NIL'"
        ).unwrap();
        assert_eq!(creating_id, "main");

        // Step 2: Hub sends spawning_ptys lifecycle event
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "main",
            "status": "spawning_ptys",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();
        let stage = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_stage or 'NIL'"
        ).unwrap();
        assert_eq!(stage, "spawning_agent");

        // Step 3: Hub sends agent_created event
        let event = serde_json::json!({
            "type": "agent_created",
            "agent": {
                "id": "my-repo-main",
                "display_name": "main",
                "repo": "my/repo",
                "branch_name": "main",
                "worktree_path": "/home/user/repo",
                "status": "running",
                "sessions": [{ "name": "agent", "port_forward": false }],
                "created_at": 1707833400,
            }
        });
        let ops = lua.call_on_hub_event("agent_created", &event, &ctx).unwrap();

        // Verify: agent is in state
        let count = lua.eval_usize("return #_tui_state.agents").unwrap();
        assert_eq!(count, 1, "Agent should be in _tui_state.agents");

        // Verify: creating indicator cleared
        let creating = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_id and 'SET' or 'NIL'"
        ).unwrap();
        assert_eq!(creating, "NIL", "Creating indicator should be cleared");

        // Verify: ops auto-focus the agent
        let ops = ops.unwrap();
        assert!(ops.iter().any(|op| op["op"] == "focus_terminal"),
            "Should auto-focus the new agent");
        assert!(ops.iter().any(|op| op["op"] == "set_mode" && op["mode"] == "insert"),
            "Should enter insert mode");

        // Verify: layout renders the agent
        let render_ctx = make_test_ctx("insert");
        let tree = lua.call_render(&render_ctx).unwrap();
        let items = extract_sidebar_items(&tree);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "main", "Sidebar should show the agent");
    }

    /// Full end-to-end: new worktree flow with all lifecycle stages visible.
    #[test]
    fn test_full_visual_flow_create_agent_new_worktree() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // User creates agent with new worktree
        lua.call_on_action("open_menu", &ctx).unwrap();
        let menu_ctx = ActionContext {
            overlay_actions: vec!["new_agent".to_string()],
            ..Default::default()
        };
        lua.call_on_action("list_select", &menu_ctx).unwrap(); // → profile select
        // Simulate single-profile response (auto-skips to worktree)
        let profiles_event = serde_json::json!({ "profiles": ["claude"] });
        lua.call_on_hub_event("profiles", &profiles_event, &ctx).unwrap();
        lua.exec("_tui_state.list_selected = 1").unwrap(); // Create New Worktree
        lua.call_on_action("list_select", &ctx).unwrap();
        lua.exec("_tui_state.input_buffer = '42'").unwrap();
        lua.call_on_action("input_submit", &ctx).unwrap(); // → branch input
        lua.exec("_tui_state.input_buffer = ''").unwrap();
        lua.call_on_action("input_submit", &ctx).unwrap(); // → empty prompt OK

        // Lifecycle: creating_worktree
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "42",
            "status": "creating_worktree",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();

        // Verify creating indicator renders
        let render_ctx = make_test_ctx("normal");
        let tree = lua.call_render(&render_ctx).unwrap();
        let items = extract_sidebar_items(&tree);
        assert!(!items.is_empty(), "Should show creating indicator");
        assert!(items[0].contains("42"), "Should show agent identifier: {}", items[0]);
        assert!(items[0].contains("Creating worktree"), "Should show stage: {}", items[0]);

        // Lifecycle: spawning_ptys
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "42",
            "status": "spawning_ptys",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();

        let tree = lua.call_render(&render_ctx).unwrap();
        let items = extract_sidebar_items(&tree);
        assert!(items[0].contains("Starting agent"),
            "Should show 'Starting agent' stage: {}", items[0]);

        // agent_created
        let event = serde_json::json!({
            "type": "agent_created",
            "agent": {
                "id": "my-repo-42",
                "display_name": "botster-issue-42",
                "repo": "my/repo",
                "issue_number": 42,
                "branch_name": "botster-issue-42",
                "worktree_path": "/tmp/worktrees/botster-issue-42",
                "status": "running",
                "sessions": [{ "name": "agent", "port_forward": false }],
                "created_at": 1707833400,
            }
        });
        lua.call_on_hub_event("agent_created", &event, &ctx).unwrap();

        // Creating indicator gone, agent in list
        let tree = lua.call_render(&render_ctx).unwrap();
        let items = extract_sidebar_items(&tree);
        assert_eq!(items.len(), 1, "Should have 1 agent, no creating indicator");
        assert_eq!(items[0], "botster-issue-42",
            "Should show agent display name, not creating indicator");
    }

    /// Failed creation should clear the creating indicator.
    #[test]
    fn test_lifecycle_failed_clears_creating_indicator() {
        let lua = make_full_lua_with_events();
        let ctx = ActionContext::default();

        // Set creating state
        lua.exec(r#"
            _tui_state.pending_fields.creating_agent_id = "my-repo-feature"
            _tui_state.pending_fields.creating_agent_stage = "creating_worktree"
        "#).unwrap();

        // Hub sends failed
        let event = serde_json::json!({
            "type": "agent_status_changed",
            "agent_id": "my-repo-feature",
            "status": "failed",
        });
        lua.call_on_hub_event("agent_status_changed", &event, &ctx).unwrap();

        let creating = lua.eval_string(
            "return _tui_state.pending_fields.creating_agent_id and 'SET' or 'NIL'"
        ).unwrap();
        assert_eq!(creating, "NIL", "Failed status should clear creating indicator");
    }

    // ========================================================================
    // Create Agent Flow Tests
    //
    // These tests prove the full create-agent state machine by driving the
    // Lua keybinding + actions layers through each step of the flow.
    // Three scenarios: main branch, new worktree, existing worktree.
    // ========================================================================

    /// Helper: open the menu and select "new_agent" to enter worktree selection.
    /// Returns the ops from the "new_agent" selection.
    fn enter_new_agent_flow(lua: &LayoutLua) -> Vec<serde_json::Value> {
        // Step 1: Open menu (ctrl+p → open_menu action)
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("open_menu", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "menu");

        // Step 2: Select "new_agent" from menu (list_select with overlay_actions)
        let ctx = ActionContext {
            overlay_actions: vec![
                "new_agent".to_string(),
                "close_agent".to_string(),
                "show_connection_code".to_string(),
            ],
            ..Default::default()
        };
        // list_selected defaults to 0 (first item = new_agent)
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "new_agent_select_profile");
        // Should send list_profiles message
        assert_eq!(ops[1]["op"], "send_msg");
        let msg_data = &ops[1]["data"]["data"];
        assert_eq!(msg_data["type"], "list_profiles");

        // Step 3: Simulate profiles event with single profile (auto-skips to worktree)
        let event_data = serde_json::json!({ "profiles": ["claude"] });
        let event_ops = lua.call_on_hub_event("profiles", &event_data, &ctx).unwrap().unwrap();
        assert_eq!(event_ops[0]["op"], "set_mode");
        assert_eq!(event_ops[0]["mode"], "new_agent_select_worktree");
        assert_eq!(event_ops[1]["op"], "send_msg");
        let msg_data = &event_ops[1]["data"]["data"];
        assert_eq!(msg_data["type"], "list_worktrees");

        event_ops
    }

    /// Helper: Enter new-agent flow with multiple profiles (requires manual selection).
    fn enter_new_agent_flow_multi_profile(lua: &LayoutLua) -> Vec<serde_json::Value> {
        // Steps 1-2: Open menu → select new_agent → enters profile selection
        let ctx = ActionContext::default();
        lua.call_on_action("open_menu", &ctx).unwrap().unwrap();

        let ctx = ActionContext {
            overlay_actions: vec!["new_agent".to_string()],
            ..Default::default()
        };
        lua.call_on_action("list_select", &ctx).unwrap();

        // Step 3: Multi-profile response — stays in profile selection
        let event_data = serde_json::json!({ "profiles": ["claude", "web"] });
        let event_ops = lua.call_on_hub_event("profiles", &event_data, &ctx).unwrap().unwrap();
        // Multi-profile: no mode change, just populates list
        assert!(event_ops.is_empty(),
            "Multi-profile should return empty ops (mode stays new_agent_select_profile)");

        let mode = lua.eval_string("return _tui_state.mode").unwrap();
        assert_eq!(mode, "new_agent_select_profile");

        event_ops
    }

    /// Multi-profile: User selects second profile, then completes flow.
    #[test]
    fn test_create_agent_flow_multi_profile_selection() {
        let lua = make_full_lua();
        enter_new_agent_flow_multi_profile(&lua);

        // Select second profile (index 1 = "web")
        lua.exec("_tui_state.list_selected = 1").unwrap();
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();

        // Should transition to worktree selection
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "new_agent_select_worktree");

        // Profile should be stored in pending_fields
        let profile = lua.eval_string("return _tui_state.pending_fields.profile").unwrap();
        assert_eq!(profile, "web");

        // Select "Use Main Branch"
        lua.exec("_tui_state.list_selected = 0").unwrap();
        lua.call_on_action("list_select", &ctx).unwrap();

        // Submit with a prompt
        lua.exec("_tui_state.input_buffer = 'test prompt'").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();

        // Verify create_agent includes the selected profile
        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert_eq!(data["profile"], "web",
            "Selected profile should be included in create_agent message");
        assert_eq!(data["prompt"], "test prompt");
    }

    /// Scenario 1: Open agent on main branch.
    ///
    /// Flow: menu → new_agent → select "Use Main Branch" (index 0) →
    ///       enter prompt → submit → create_agent with issue_or_branch=null
    #[test]
    fn test_create_agent_flow_main_branch() {
        let lua = make_full_lua();

        // Enter the new agent flow (menu → new_agent → worktree selection)
        enter_new_agent_flow(&lua);

        // Step 3: Select "Use Main Branch" (list_selected = 0, the default)
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "new_agent_prompt",
            "Selecting 'Use Main Branch' should transition to prompt mode");

        // Verify pending state: use_main_branch should be set
        let use_main = lua.eval_string("return _tui_state.pending_fields.use_main_branch").unwrap();
        assert_eq!(use_main, "true");

        // pending_issue_or_branch should be nil (main branch has no issue)
        let has_issue = lua.eval_string(
            "return _tui_state.pending_fields.pending_issue_or_branch and 'set' or 'nil'"
        ).unwrap();
        assert_eq!(has_issue, "nil");

        // Step 4: Type a prompt and submit
        lua.exec("_tui_state.input_buffer = 'Fix the login bug'").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();

        // Should send create_agent message
        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert!(data["issue_or_branch"].is_null(),
            "Main branch mode should have null issue_or_branch");
        assert_eq!(data["prompt"], "Fix the login bug");

        // Should transition back to base mode (normal, since no agent selected)
        let mode_op = ops.iter().find(|op| op["op"] == "set_mode").unwrap();
        assert_eq!(mode_op["mode"], "normal");

        // Verify creating_agent_id was set for progress tracking
        let creating_id = lua.eval_string("return _tui_state.pending_fields.creating_agent_id").unwrap();
        assert_eq!(creating_id, "main");

        // Verify creating_agent_stage is "spawning" (main skips worktree creation)
        let stage = lua.eval_string("return _tui_state.pending_fields.creating_agent_stage").unwrap();
        assert_eq!(stage, "spawning");
    }

    /// Scenario 1b: Open agent on main with empty prompt (optional prompt).
    #[test]
    fn test_create_agent_flow_main_branch_no_prompt() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        // Select "Use Main Branch"
        let ctx = ActionContext::default();
        lua.call_on_action("list_select", &ctx).unwrap();

        // Submit with empty prompt
        lua.exec("_tui_state.input_buffer = ''").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();

        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert!(data["prompt"].is_null(),
            "Empty prompt should be sent as null (handler applies default)");
    }

    /// Scenario 2: Open agent in new worktree (branch name).
    ///
    /// Flow: menu → new_agent → select "Create New Worktree" (index 1) →
    ///       enter branch name → enter prompt → submit → create_agent
    #[test]
    fn test_create_agent_flow_new_worktree_branch() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        // Step 3: Select "Create New Worktree" (list_selected = 1)
        lua.exec("_tui_state.list_selected = 1").unwrap();
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "new_agent_create_worktree",
            "Selecting 'Create New Worktree' should enter branch name input");

        // Verify use_main_branch is cleared
        let use_main = lua.eval_string(
            "return _tui_state.pending_fields.use_main_branch and 'set' or 'nil'"
        ).unwrap();
        assert_eq!(use_main, "nil");

        // Step 4: Type branch name and submit
        lua.exec("_tui_state.input_buffer = 'feature-auth'").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "new_agent_prompt",
            "Submitting branch name should transition to prompt mode");

        // Verify branch was stored
        let branch = lua.eval_string(
            "return _tui_state.pending_fields.pending_issue_or_branch"
        ).unwrap();
        assert_eq!(branch, "feature-auth");

        // Step 5: Type prompt and submit
        lua.exec("_tui_state.input_buffer = 'Implement OAuth login'").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();

        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert_eq!(data["issue_or_branch"], "feature-auth");
        assert_eq!(data["prompt"], "Implement OAuth login");

        // Should set creating_agent_stage to "creating_worktree" (not "spawning")
        let stage = lua.eval_string("return _tui_state.pending_fields.creating_agent_stage").unwrap();
        assert_eq!(stage, "creating_worktree",
            "New worktree flow should set stage to creating_worktree");
    }

    /// Scenario 2b: Open agent in new worktree (issue number).
    #[test]
    fn test_create_agent_flow_new_worktree_issue_number() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        // Select "Create New Worktree"
        lua.exec("_tui_state.list_selected = 1").unwrap();
        let ctx = ActionContext::default();
        lua.call_on_action("list_select", &ctx).unwrap();

        // Type issue number
        lua.exec("_tui_state.input_buffer = '42'").unwrap();
        lua.call_on_action("input_submit", &ctx).unwrap();

        // Type prompt and submit
        lua.exec("_tui_state.input_buffer = 'Fix reported crash'").unwrap();
        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();

        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert_eq!(data["issue_or_branch"], "42",
            "Issue number should be passed as-is (handler parses it)");
        assert_eq!(data["prompt"], "Fix reported crash");
    }

    /// Scenario 2c: Empty branch name should NOT advance.
    #[test]
    fn test_create_agent_flow_new_worktree_empty_branch_rejected() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        lua.exec("_tui_state.list_selected = 1").unwrap();
        let ctx = ActionContext::default();
        lua.call_on_action("list_select", &ctx).unwrap();

        // Submit with empty branch name — should return nil (no-op)
        lua.exec("_tui_state.input_buffer = ''").unwrap();
        let result = lua.call_on_action("input_submit", &ctx).unwrap();
        assert!(result.is_none(),
            "Empty branch name should not advance the flow");

        // Mode should still be new_agent_create_worktree
        let mode = lua.eval_string("return _tui_state.mode").unwrap();
        assert_eq!(mode, "new_agent_create_worktree");
    }

    /// Scenario 3: Open agent in existing worktree.
    ///
    /// Flow: menu → new_agent → select existing worktree (index 2+) →
    ///       sends reopen_worktree directly (no prompt step)
    #[test]
    fn test_create_agent_flow_existing_worktree() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        // Set up available_worktrees (populated by list_worktrees response)
        lua.exec(r#"
            _tui_state.available_worktrees = {
                { branch = "feature-auth", path = "/tmp/worktrees/feature-auth" },
                { branch = "bugfix-login", path = "/tmp/worktrees/bugfix-login" },
            }
        "#).unwrap();

        // Select first existing worktree (index 2 in the list:
        //   0 = "Use Main Branch", 1 = "Create New Worktree", 2 = first worktree)
        lua.exec("_tui_state.list_selected = 2").unwrap();
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();

        // Should send reopen_worktree message (NOT create_agent — no prompt step)
        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "reopen_worktree");
        assert_eq!(data["path"], "/tmp/worktrees/feature-auth");
        assert_eq!(data["branch"], "feature-auth");

        // Should set creating_agent_id for progress tracking
        let creating_id = lua.eval_string("return _tui_state.pending_fields.creating_agent_id").unwrap();
        assert_eq!(creating_id, "feature-auth");

        // Should set stage to creating_worktree
        let stage = lua.eval_string("return _tui_state.pending_fields.creating_agent_stage").unwrap();
        assert_eq!(stage, "creating_worktree");

        // Should transition back to base mode (skips prompt entirely)
        let mode_op = ops.iter().find(|op| op["op"] == "set_mode").unwrap();
        assert_eq!(mode_op["mode"], "normal",
            "Existing worktree should return to normal mode (no prompt step)");
    }

    /// Scenario 3b: Select second existing worktree.
    #[test]
    fn test_create_agent_flow_existing_worktree_second_item() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        lua.exec(r#"
            _tui_state.available_worktrees = {
                { branch = "feature-auth", path = "/tmp/worktrees/feature-auth" },
                { branch = "bugfix-login", path = "/tmp/worktrees/bugfix-login" },
            }
        "#).unwrap();

        // Select second existing worktree (index 3)
        lua.exec("_tui_state.list_selected = 3").unwrap();
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("list_select", &ctx).unwrap().unwrap();

        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "reopen_worktree");
        assert_eq!(data["path"], "/tmp/worktrees/bugfix-login");
        assert_eq!(data["branch"], "bugfix-login");
    }

    /// Escape at any point in the flow should return to base mode.
    #[test]
    fn test_create_agent_flow_escape_cancels() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        // Escape from worktree selection
        let ctx = ActionContext::default();
        let ops = lua.call_on_action("close_modal", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["op"], "set_mode");
        assert_eq!(ops[0]["mode"], "normal",
            "Escape should return to normal mode (no agent selected)");

        // Start over and escape from branch name input
        enter_new_agent_flow(&lua);
        lua.exec("_tui_state.list_selected = 1").unwrap();
        lua.call_on_action("list_select", &ctx).unwrap();
        let ops = lua.call_on_action("close_modal", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["mode"], "normal");

        // Start over and escape from prompt input
        enter_new_agent_flow(&lua);
        lua.call_on_action("list_select", &ctx).unwrap(); // Use Main Branch
        let ops = lua.call_on_action("close_modal", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["mode"], "normal");
    }

    /// When an agent is selected, escape returns to insert mode (not normal).
    #[test]
    fn test_create_agent_flow_escape_returns_to_insert_when_agent_selected() {
        let lua = make_full_lua();
        enter_new_agent_flow(&lua);

        let ctx = ActionContext {
            selected_agent: Some("test-agent-1".to_string()),
            ..Default::default()
        };
        let ops = lua.call_on_action("close_modal", &ctx).unwrap().unwrap();
        assert_eq!(ops[0]["mode"], "insert",
            "Escape with selected agent should return to insert mode");
    }

    /// Full keypress-level test: ctrl+p → enter → enter → type → enter.
    /// Proves the flow works end-to-end through handle_key → on_action.
    #[test]
    fn test_create_agent_flow_main_branch_via_keypresses() {
        let lua = make_full_lua();
        let key_ctx = KeyContext { list_count: 3, terminal_rows: 24 };

        // ctrl+p in normal mode → open_menu
        let action = lua.call_handle_key("ctrl+p", "normal", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "open_menu");

        // Dispatch open_menu → mode becomes "menu"
        let ctx = ActionContext::default();
        lua.call_on_action("open_menu", &ctx).unwrap();

        // enter in menu mode → list_select
        let action = lua.call_handle_key("enter", "menu", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "list_select");

        // Dispatch list_select with new_agent → mode becomes "new_agent_select_profile"
        let ctx = ActionContext {
            overlay_actions: vec!["new_agent".to_string()],
            ..Default::default()
        };
        lua.call_on_action("list_select", &ctx).unwrap();

        // Simulate single-profile response (auto-skips to worktree selection)
        let profiles_event = serde_json::json!({ "profiles": ["claude"] });
        lua.call_on_hub_event("profiles", &profiles_event, &ctx).unwrap();

        // enter in worktree selection → list_select (selects "Use Main Branch")
        let action = lua.call_handle_key("enter", "new_agent_select_worktree", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "list_select");
        let ctx = ActionContext::default();
        lua.call_on_action("list_select", &ctx).unwrap();

        // Type prompt characters
        let action = lua.call_handle_key("H", "new_agent_prompt", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "input_char");
        assert_eq!(action.char, Some('H'));

        let action = lua.call_handle_key("i", "new_agent_prompt", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "input_char");

        // Simulate typed text
        lua.exec("_tui_state.input_buffer = 'Hi'").unwrap();

        // enter in prompt mode → input_submit
        let action = lua.call_handle_key("enter", "new_agent_prompt", &key_ctx).unwrap().unwrap();
        assert_eq!(action.action, "input_submit");

        let ops = lua.call_on_action("input_submit", &ctx).unwrap().unwrap();
        let send_op = ops.iter().find(|op| op["op"] == "send_msg").unwrap();
        let data = &send_op["data"]["data"];
        assert_eq!(data["type"], "create_agent");
        assert_eq!(data["prompt"], "Hi");
    }
}
