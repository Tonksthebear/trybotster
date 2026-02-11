//! TUI-owned Lua state for declarative layout rendering and keybinding dispatch.
//!
//! Creates a separate, lightweight `mlua::Lua` state owned by TuiRunner's thread.
//! This avoids threading issues (the Hub's LuaRuntime is `!Send`).
//!
//! Loads two Lua modules:
//! - `ui/layout.lua` — calls `render(state)` and `render_overlay(state)` each frame
//! - `ui/keybindings.lua` — calls `handle_key(descriptor, mode, context)` per keypress

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
/// Contains state that keybinding fallback logic needs (e.g., menu item
/// count for number shortcut validation).
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    /// Currently selected menu item index.
    pub menu_selected: usize,
    /// Total number of selectable menu items.
    pub menu_count: usize,
    /// Currently selected worktree index.
    pub worktree_selected: usize,
    /// Terminal height in rows (for scroll amount calculation).
    pub terminal_rows: u16,
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
        set_field(&ctx_table, "menu_selected", context.menu_selected)?;
        set_field(&ctx_table, "menu_count", context.menu_count)?;
        set_field(&ctx_table, "worktree_selected", context.worktree_selected)?;
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

    // Mode (as lowercase string for Lua matching)
    let mode_str = match ctx.mode {
        crate::app::AppMode::Normal => "normal",
        crate::app::AppMode::Menu => "menu",
        crate::app::AppMode::NewAgentSelectWorktree => "new_agent_select_worktree",
        crate::app::AppMode::NewAgentCreateWorktree => "new_agent_create_worktree",
        crate::app::AppMode::NewAgentPrompt => "new_agent_prompt",
        crate::app::AppMode::CloseAgentConfirm => "close_agent_confirm",
        crate::app::AppMode::ConnectionCode => "connection_code",
        crate::app::AppMode::Error => "error",
    };
    set_field(&state, "mode", mode_str)?;

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
    set_field(&state, "menu_selected", ctx.menu_selected)?;
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

    // Creating agent info
    if let Some((identifier, stage)) = &ctx.creating_agent {
        let creating = lua
            .create_table()
            .map_err(|e| anyhow!("Failed to create creating_agent table: {e}"))?;
        set_field(&creating, "identifier", *identifier)?;
        let stage_str = match stage {
            super::events::CreationStage::CreatingWorktree => "creating_worktree",
            super::events::CreationStage::CopyingConfig => "copying_config",
            super::events::CreationStage::SpawningAgent => "spawning_agent",
            super::events::CreationStage::Ready => "ready",
        };
        set_field(&creating, "stage", stage_str)?;
        state
            .set("creating_agent", creating)
            .map_err(|e| anyhow!("Failed to set creating_agent: {e}"))?;
    }

    // Worktree selection
    set_field(&state, "worktree_selected", ctx.worktree_selected)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppMode;
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
                        { type = "agent_list" },
                        { type = "terminal" },
                    }
                }
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
                        { type = "agent_list", block = { title = title, borders = "all" } },
                        { type = "terminal" },
                    }
                }
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
                        child = { type = "menu", block = { title = " Menu ", borders = "all" } }
                    }
                end
                return nil
            end
        "#,
        )
        .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: AppMode::Menu,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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

    /// Verifies that the actual layout.lua handles every AppMode correctly.
    ///
    /// This is the key consistency test: Rust serializes mode strings in
    /// `render_context_to_lua()`, and layout.lua must handle each one.
    /// If a new AppMode is added but not handled in layout.lua, this test
    /// catches the drift.
    #[test]
    fn test_mode_string_consistency_with_actual_layout() {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let layout = LayoutLua::new(layout_source).expect("actual layout.lua should load");

        let (agents, agent_ids) = make_test_ctx();

        // Map of every AppMode to whether it should produce an overlay
        let mode_expectations: Vec<(AppMode, bool, &str)> = vec![
            (AppMode::Normal, false, "normal"),
            (AppMode::Menu, true, "menu"),
            (AppMode::NewAgentSelectWorktree, true, "new_agent_select_worktree"),
            (AppMode::NewAgentCreateWorktree, true, "new_agent_create_worktree"),
            (AppMode::NewAgentPrompt, true, "new_agent_prompt"),
            (AppMode::CloseAgentConfirm, true, "close_agent_confirm"),
            (AppMode::ConnectionCode, true, "connection_code"),
            (AppMode::Error, true, "error"),
        ];

        for (mode, expect_overlay, label) in &mode_expectations {
            let ctx = RenderContext {
                mode: *mode,
                menu_selected: 0,
                input_buffer: "",
                worktree_selected: 0,
                available_worktrees: &[],
                error_message: Some("test error"),
                creating_agent: None,
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
        let mode_widgets: Vec<(AppMode, &str)> = vec![
            (AppMode::Menu, "Menu"),
            (AppMode::NewAgentSelectWorktree, "WorktreeSelect"),
            (AppMode::NewAgentCreateWorktree, "TextInput"),
            (AppMode::NewAgentPrompt, "TextInput"),
            (AppMode::CloseAgentConfirm, "CloseConfirm"),
            (AppMode::ConnectionCode, "ConnectionCode"),
            (AppMode::Error, "Error"),
        ];

        for (mode, expected_widget) in &mode_widgets {
            let ctx = RenderContext {
                mode: *mode,
                menu_selected: 0,
                input_buffer: "",
                worktree_selected: 0,
                available_worktrees: &[],
                error_message: Some("test"),
                creating_agent: None,
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
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
                        { type = "agent_list" },
                        { type = "terminal" },
                    }
                }
            end
        "#,
            )
            .unwrap();

        let (agents, agent_ids) = make_test_ctx();
        let ctx = RenderContext {
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: "",
            worktree_selected: 0,
            available_worktrees: &[],
            error_message: None,
            creating_agent: None,
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
