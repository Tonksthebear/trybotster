//! TUI adapter for the cross-client UI DSL (Phase B).
//!
//! The adapter translates a [`UiNode`] tree (produced by Phase A's Lua
//! DSL registered in both the hub VM and the TUI's `LayoutLua`) into the
//! existing TUI [`RenderNode`] / [`WidgetType`] tree, without altering the
//! legacy render path. See the knowledge-vault note
//! `tui adapter maps shared primitives onto existing rust render tree
//! without flag day rewrite` for the motivating architecture.
//!
//! # Entry points
//!
//! - [`render_ui_node`] — walk a `UiNode` tree, producing a `RenderNode`
//!   and an [`ActionTable`] that preserves full action envelopes.
//! - [`render_lua_ui_node`] — convenience wrapper that pulls a node straight
//!   out of a Lua `mlua::Table`, used by `LayoutLua::call_render`.
//! - [`derive_viewport_from_terminal`] — produce a [`UiViewport`] from
//!   terminal columns / rows.
//! - [`is_ui_node_type`] — cheap shape check so callers can decide
//!   whether a given Lua layout table is in the Phase A shape.
//!
//! # What stays untouched
//!
//! The legacy TUI code that emits raw `{ type = "hsplit", ... }` tables
//! keeps working: [`RenderNode::from_lua_table`] still handles those
//! shapes. The adapter is opt-in — only Lua code that returns a Phase A
//! tree (top-level `type` equal to a recognised primitive) flows through
//! this module.
//!
//! # Phase B scope
//!
//! The adapter maps the 12 Lua-public primitives plus the internal
//! `dialog` / `menu` pair, and resolves adaptive-spec sentinels
//! (`$kind = "responsive"`, `$kind = "when"`, `$kind = "hidden"`).
//! Neither the render tree nor the Lua DSL itself are modified here —
//! see the README in `cli/src/ui_contract/` for the Phase A surface this
//! module consumes.
//!
//! [`RenderNode`]: crate::tui::render_tree::RenderNode
//! [`UiNode`]: crate::ui_contract::node::UiNode
//! [`UiViewport`]: crate::ui_contract::viewport::UiViewport
//! [`WidgetType`]: crate::tui::render_tree::WidgetType

// Rust guideline compliant 2026-04-18

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Table as LuaTable, Value as LuaValue};
use serde_json::Value as JsonValue;

use crate::tui::entity_stores::TuiEntityStores;
use crate::tui::render_tree::RenderNode;
use crate::ui_contract::node::UiNode;
use crate::ui_contract::viewport::UiViewport;

pub mod action;
pub mod binding;
pub mod primitive;
pub mod responsive;
pub mod style;
pub mod viewport;

pub use action::ActionTable;
pub use binding::resolve_bindings;
pub use primitive::{is_ui_node_type, render_ui_node, render_ui_node_with_stores};
pub use viewport::{derive_viewport_from_terminal, height_class_for_rows, width_class_for_cols};

/// Render a Phase A [`UiNode`] tree from a Lua table directly into a
/// [`RenderNode`].
///
/// Convenience entry point for `LayoutLua` — the runner holds a Lua VM
/// plus the `mlua::Table` returned by the Lua `render(state)` function
/// and just needs a `RenderNode` back. The returned [`ActionTable`]
/// preserves the full Phase A action envelopes for callers that want
/// payload-aware dispatch; callers that only care about the rendered
/// tree may discard it.
///
/// # Errors
///
/// Returns an error if the table does not deserialise into a
/// [`UiNode`] (for example, because the top-level `type` does not
/// name a known primitive), or if any primitive renderer fails.
pub fn render_lua_ui_node(
    lua: &Lua,
    table: &LuaTable,
    viewport: &UiViewport,
) -> Result<(RenderNode, ActionTable)> {
    render_lua_ui_node_with_stores(lua, table, viewport, None)
}

/// Render a Lua-built UiNode tree with optional access to the entity
/// stores for binding resolution + composite data.
///
/// The pipeline is:
///
/// 1. Lua table → `serde_json::Value` (preserving the wire shape including
///    any `$bind` / `$kind = "bind_list"` sentinels).
/// 2. [`resolve_bindings`] walks the tree replacing every sentinel with
///    the resolved value(s) from `stores`. Skipped when `stores` is None.
/// 3. JSON → [`UiNode`].
/// 4. [`render_ui_node_with_stores`] dispatches to the per-primitive
///    renderer with `stores` threaded so composites
///    (`session_list`, `workspace_list`, …) can read their data.
///
/// # Errors
///
/// Returns an error if the table does not deserialise into a
/// [`UiNode`] post-binding-resolution, or if a primitive renderer fails.
pub fn render_lua_ui_node_with_stores(
    lua: &Lua,
    table: &LuaTable,
    viewport: &UiViewport,
    stores: Option<&TuiEntityStores>,
) -> Result<(RenderNode, ActionTable)> {
    // Step 1: Lua → JSON (preserves $bind / $kind sentinels verbatim).
    let mut json: JsonValue = lua
        .from_value(LuaValue::Table(table.clone()))
        .map_err(|e| anyhow!("ui_contract_adapter: Lua → JSON failed: {e}"))?;

    // Step 2: resolve sentinels in-place when stores are available.
    if let Some(stores) = stores {
        resolve_bindings(&mut json, stores);
    }

    // Step 3: JSON → typed node.
    let node: UiNode = serde_json::from_value(json)
        .map_err(|e| anyhow!("ui_contract_adapter: JSON → UiNode failed: {e}"))?;

    // Step 4: dispatch.
    let mut actions = ActionTable::new();
    let rendered = render_ui_node_with_stores(&node, viewport, &mut actions, stores)?;
    Ok((rendered, actions))
}
