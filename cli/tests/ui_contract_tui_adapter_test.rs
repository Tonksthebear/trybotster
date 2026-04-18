//! Integration tests for the Phase B TUI adapter
//! (`crate::tui::ui_contract_adapter`).
//!
//! Each test goes end-to-end:
//!
//! 1. Register the Phase A Lua DSL in a fresh `mlua::Lua`.
//! 2. Evaluate Lua source that returns a `UiNodeV1` tree.
//! 3. Run the adapter against that tree for a chosen [`UiViewportV1`].
//! 4. Assert on the resulting [`RenderNode`] shape / content and the
//!    associated [`ActionTable`].
//!
//! Run with:
//!
//! ```sh
//! cd cli
//! BOTSTER_ENV=test cargo test --test ui_contract_tui_adapter_test
//! ```

#![expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "UiViewportV1 is Copy but helpers pass it by reference to match the adapter's signatures — consistency is more valuable here than the trivial copy optimisation"
)]

use botster::tui::render_tree::{
    ListProps, ParagraphProps, RenderNode, StyledContent, WidgetProps, WidgetType,
};
use botster::tui::ui_contract_adapter::{
    derive_viewport_from_terminal, render_lua_ui_node, render_ui_node, ActionTable,
};
use botster::ui_contract::lua::register;
use botster::ui_contract::node::UiNodeV1;
use botster::ui_contract::viewport::{UiHeightClass, UiPointer, UiViewportV1, UiWidthClass};
use mlua::{Lua, LuaSerdeExt, Value};

fn new_ui_lua() -> Lua {
    let lua = Lua::new();
    register(&lua).expect("register ui primitives");
    lua
}

fn regular_viewport() -> UiViewportV1 {
    UiViewportV1::new(
        UiWidthClass::Regular,
        UiHeightClass::Regular,
        UiPointer::None,
    )
}

fn compact_viewport() -> UiViewportV1 {
    UiViewportV1::new(
        UiWidthClass::Compact,
        UiHeightClass::Regular,
        UiPointer::None,
    )
}

fn expanded_viewport() -> UiViewportV1 {
    UiViewportV1::new(
        UiWidthClass::Expanded,
        UiHeightClass::Regular,
        UiPointer::None,
    )
}

/// Run Lua code and route its result through the adapter.
fn render_lua(lua: &Lua, code: &str, viewport: &UiViewportV1) -> (RenderNode, ActionTable) {
    let value: Value = lua.load(code).eval().expect("Lua eval failed");
    let table = match value {
        Value::Table(t) => t,
        other => panic!("expected Lua table, got {:?}", other.type_name()),
    };
    render_lua_ui_node(lua, &table, viewport).expect("render_lua_ui_node")
}

/// Convenience: evaluate Lua, deserialize to UiNodeV1, then render.
fn eval_node(lua: &Lua, code: &str) -> UiNodeV1 {
    let value: Value = lua.load(code).eval().expect("Lua eval failed");
    lua.from_value(value).expect("Lua -> UiNodeV1")
}

// =============================================================================
// Layout primitives
// =============================================================================

#[test]
fn stack_vertical_maps_to_vsplit() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.stack{
            direction = "vertical",
            children = { ui.text{ text = "hello" }, ui.text{ text = "world" } },
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::VSplit {
            constraints,
            children,
        } => {
            assert_eq!(constraints.len(), 2);
            assert_eq!(children.len(), 2);
        }
        other => panic!("expected VSplit, got {other:?}"),
    }
}

#[test]
fn stack_horizontal_maps_to_hsplit() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.stack{
            direction = "horizontal",
            children = { ui.text{ text = "a" }, ui.text{ text = "b" } },
        }"#,
        &regular_viewport(),
    );
    assert!(matches!(render, RenderNode::HSplit { .. }));
}

#[test]
fn inline_maps_to_hsplit() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.inline{
            children = { ui.text{ text = "a" }, ui.text{ text = "b" } },
        }"#,
        &regular_viewport(),
    );
    assert!(matches!(render, RenderNode::HSplit { .. }));
}

#[test]
fn panel_with_single_child_attaches_block_to_child() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.panel{
            title = "Preview",
            border = true,
            children = { ui.text{ text = "hello" } },
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::Widget { block, .. } => {
            let block = block.expect("block attached");
            match block.title.expect("title present") {
                StyledContent::Plain(t) => assert_eq!(t, "Preview"),
                StyledContent::Styled(_) => panic!("title should be plain"),
            }
        }
        other => panic!("expected Widget, got {other:?}"),
    }
}

#[test]
fn panel_with_multiple_children_renders_header_plus_vsplit() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.panel{
            title = "List",
            border = true,
            children = {
                ui.text{ text = "row1" },
                ui.text{ text = "row2" },
            },
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::VSplit { children, .. } => {
            // header + 2 children
            assert_eq!(children.len(), 3);
        }
        other => panic!("expected VSplit, got {other:?}"),
    }
}

#[test]
fn panel_nested_in_stack_renders_without_error() {
    // Regression test for orchestrator flag: panel inside a stack should
    // render as a VSplit whose children include the panel subtree. The
    // panel keeps its own (additive) chrome behavior.
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.stack{
            direction = "vertical",
            children = {
                ui.panel{
                    title = "One",
                    border = true,
                    children = { ui.text{ text = "x" } },
                },
                ui.panel{
                    title = "Two",
                    border = true,
                    children = { ui.text{ text = "y" } },
                },
            },
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::VSplit { children, .. } => assert_eq!(children.len(), 2),
        other => panic!("expected VSplit, got {other:?}"),
    }
}

#[test]
fn scroll_area_single_child_passes_through() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.scroll_area{
            children = { ui.text{ text = "body" } },
        }"#,
        &regular_viewport(),
    );
    // Single-child scroll_area is a zero-overhead pass-through.
    assert!(matches!(
        render,
        RenderNode::Widget {
            widget_type: WidgetType::Paragraph,
            ..
        }
    ));
}

// =============================================================================
// Content primitives
// =============================================================================

#[test]
fn text_renders_as_paragraph_with_single_line() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.text{ text = "hello world", weight = "semibold" }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::Widget {
            widget_type: WidgetType::Paragraph,
            props: Some(WidgetProps::Paragraph(p)),
            ..
        } => {
            assert_eq!(p.lines.len(), 1);
            // Weight = semibold → styled spans should carry bold
            match &p.lines[0] {
                StyledContent::Styled(spans) => {
                    assert_eq!(spans.len(), 1);
                    assert_eq!(spans[0].text, "hello world");
                    assert!(spans[0].style.bold);
                }
                StyledContent::Plain(_) => panic!("expected Styled, got Plain"),
            }
        }
        other => panic!("expected Paragraph widget, got {other:?}"),
    }
}

#[test]
fn icon_renders_known_glyph_and_unknown_fallback() {
    let lua = new_ui_lua();

    let (known, _) = render_lua(
        &lua,
        r#"return ui.icon{ name = "close" }"#,
        &regular_viewport(),
    );
    assert!(paragraph_text_contains(&known, "✕"));

    let (unknown, _) = render_lua(
        &lua,
        r#"return ui.icon{ name = "mystery" }"#,
        &regular_viewport(),
    );
    assert!(paragraph_text_contains(&unknown, ":mystery:"));
}

#[test]
fn badge_renders_bracketed_span() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.badge{ text = "NEW", tone = "warning" }"#,
        &regular_viewport(),
    );
    assert!(paragraph_text_contains(&render, "[NEW]"));
}

#[test]
fn status_dot_renders_filled_bullet() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.status_dot{ state = "active" }"#,
        &regular_viewport(),
    );
    assert!(paragraph_text_contains(&render, "\u{25CF}"));
}

#[test]
fn empty_state_renders_centered_stack_with_primary_action_button() {
    let lua = new_ui_lua();
    let (render, actions) = render_lua(
        &lua,
        r#"return ui.empty_state{
            title = "No sessions yet",
            description = "Spawn one to get started.",
            icon = "sparkle",
            primary_action = ui.action("botster.session.create.request"),
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::Centered { child, .. } => match *child {
            RenderNode::VSplit { children, .. } => {
                // icon + title + description + primary_action = 4 rows
                assert_eq!(children.len(), 4);
            }
            other => panic!("empty_state inner expected VSplit, got {other:?}"),
        },
        other => panic!("empty_state expected Centered, got {other:?}"),
    }
    assert!(actions
        .first_by_action_id("botster.session.create.request")
        .is_some());
}

// =============================================================================
// Action primitives
// =============================================================================

#[test]
fn button_emits_action_id_on_list_item() {
    let lua = new_ui_lua();
    let (render, actions) = render_lua(
        &lua,
        r#"return ui.button{
            label = "Save",
            action = ui.action("botster.workspace.save"),
            tone = "accent",
        }"#,
        &regular_viewport(),
    );
    let list_props = expect_list_props(&render);
    assert_eq!(list_props.items.len(), 1);
    assert_eq!(
        list_props.items[0].action.as_deref(),
        Some("botster.workspace.save")
    );
    assert!(actions
        .first_by_action_id("botster.workspace.save")
        .is_some());
}

#[test]
fn button_disabled_action_drops_list_item_action() {
    let lua = new_ui_lua();
    let (render, actions) = render_lua(
        &lua,
        r#"
        local act = ui.action("botster.session.close.request")
        act.disabled = true
        return ui.button{ label = "Close", action = act }
        "#,
        &regular_viewport(),
    );
    let list_props = expect_list_props(&render);
    assert!(list_props.items[0].action.is_none());
    // ActionTable should still record the envelope so callers can surface
    // disabled state if they want to.
    let envelope = actions
        .first_by_action_id("botster.session.close.request")
        .expect("envelope recorded");
    assert_eq!(envelope.disabled, Some(true));
}

#[test]
fn icon_button_embeds_icon_in_label() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.icon_button{
            icon = "check",
            label = "Apply",
            action = ui.action("botster.workspace.apply"),
        }"#,
        &regular_viewport(),
    );
    let list_props = expect_list_props(&render);
    // The single item content should include the icon glyph and the label.
    let plain = list_item_plain(&list_props.items[0].content);
    assert!(plain.contains("✓"), "plain={plain:?}");
    assert!(plain.contains("Apply"), "plain={plain:?}");
}

// =============================================================================
// Collection primitives
// =============================================================================

#[test]
fn list_renders_selectable_items_with_slot_text() {
    let lua = new_ui_lua();
    let node = eval_node(
        &lua,
        r#"return {
            type = "list",
            children = {
                { type = "list_item",
                  props = { selected = true, action = ui.action("botster.session.select", { sessionUuid = "sess-1" }) },
                  slots = { title = { ui.text{ text = "Primary" } },
                            subtitle = { ui.text{ text = "Secondary" } },
                            start = { ui.icon{ name = "session" } },
                            ["end"] = { ui.badge{ text = "3" } } },
                },
                { type = "list_item",
                  props = { action = ui.action("botster.session.select", { sessionUuid = "sess-2" }) },
                  slots = { title = { ui.text{ text = "Other" } } },
                },
            },
        }"#,
    );
    let mut actions = ActionTable::new();
    let render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let list_props = expect_list_props(&render);
    assert_eq!(list_props.items.len(), 2);
    assert_eq!(list_props.selected, Some(0));

    let first_plain = list_item_plain(&list_props.items[0].content);
    assert!(first_plain.contains("Primary"));
    // secondary slot lands on the secondary/subtitle line
    let secondary = list_props.items[0]
        .secondary
        .as_ref()
        .expect("secondary present");
    match secondary {
        StyledContent::Plain(s) => assert_eq!(s, "Secondary"),
        StyledContent::Styled(_) => panic!("secondary should be plain"),
    }

    assert_eq!(
        list_props.items[0].action.as_deref(),
        Some("botster.session.select")
    );
    // F1 regression: two rows with the same action id must keep both
    // envelopes (one per row) instead of collapsing to last-write-wins.
    let envelopes: Vec<_> = actions.by_action_id("botster.session.select").collect();
    assert_eq!(envelopes.len(), 2, "per-row envelopes must NOT collapse");
    assert_eq!(
        envelopes[0].action.payload.get("sessionUuid"),
        Some(&serde_json::json!("sess-1"))
    );
    assert_eq!(
        envelopes[1].action.payload.get("sessionUuid"),
        Some(&serde_json::json!("sess-2"))
    );
}

#[test]
fn tree_renders_three_depths_with_indentation() {
    // Orchestrator flag: tree with 3 levels should produce visually
    // distinct depths. We verify by scanning the indentation prefixes.
    let lua = new_ui_lua();
    let node = eval_node(
        &lua,
        r#"return ui.tree{
            children = {
                ui.tree_item{
                    id = "l1-a", expanded = true,
                    title = { ui.text{ text = "Level 1 A" } },
                    children = {
                        ui.tree_item{
                            id = "l2-a", expanded = true,
                            title = { ui.text{ text = "Level 2 A" } },
                            children = {
                                ui.tree_item{
                                    id = "l3-a",
                                    title = { ui.text{ text = "Level 3 A" } },
                                },
                            },
                        },
                    },
                },
                ui.tree_item{
                    id = "l1-b", notification = true,
                    title = { ui.text{ text = "Level 1 B" } },
                },
            },
        }"#,
    );
    let mut actions = ActionTable::new();
    let render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let list = expect_list_props(&render);
    // Three expanded rows from depth 0→2, plus a top-level sibling.
    assert_eq!(list.items.len(), 4);

    let depth0 = list_item_plain(&list.items[0].content);
    let depth1 = list_item_plain(&list.items[1].content);
    let depth2 = list_item_plain(&list.items[2].content);
    let sibling = list_item_plain(&list.items[3].content);

    // Depth 0 starts with no `└` prefix, depth 1 has one `└`, depth 2 has
    // leading padding plus `└`.
    assert!(!depth0.contains('└'), "row0={depth0:?}");
    assert!(depth1.contains('└'), "row1={depth1:?}");
    assert!(depth2.contains('└'), "row2={depth2:?}");
    // Depth 2 has strictly more leading whitespace than depth 1.
    assert!(
        depth2.chars().take_while(|c| *c == ' ').count()
            > depth1.chars().take_while(|c| *c == ' ').count(),
        "depth2 should be more indented than depth1: depth1={depth1:?} depth2={depth2:?}"
    );

    // Notification row renders the yellow bullet glyph in its content.
    assert!(sibling.contains('●'), "sibling row missing notification marker: {sibling:?}");
}

#[test]
fn tree_collapsed_item_elides_children() {
    let lua = new_ui_lua();
    let node = eval_node(
        &lua,
        r#"return ui.tree{
            children = {
                ui.tree_item{
                    id = "parent", expanded = false,
                    title = { ui.text{ text = "Parent" } },
                    children = {
                        ui.tree_item{ id = "child", title = { ui.text{ text = "Child" } } },
                    },
                },
            },
        }"#,
    );
    let mut actions = ActionTable::new();
    let render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let list = expect_list_props(&render);
    assert_eq!(list.items.len(), 1, "collapsed tree should elide children");
}

// =============================================================================
// Dialog
// =============================================================================

#[test]
fn dialog_open_renders_centered_stack_with_title() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.dialog{
            open = true,
            title = "Rename Workspace",
            body = { ui.text{ text = "New name:" } },
            footer = {
                ui.button{ label = "Cancel", action = ui.action("botster.dialog.cancel") },
                ui.button{ label = "Apply",  action = ui.action("botster.workspace.rename.apply") },
            },
        }"#,
        &regular_viewport(),
    );
    match render {
        RenderNode::Centered { child, .. } => match *child {
            RenderNode::VSplit { children, .. } => {
                // title + body + footer
                assert_eq!(children.len(), 3);
            }
            other => panic!("dialog inner expected VSplit, got {other:?}"),
        },
        other => panic!("dialog expected Centered, got {other:?}"),
    }
}

#[test]
fn dialog_closed_renders_empty_placeholder() {
    let lua = new_ui_lua();
    let (render, _) = render_lua(
        &lua,
        r#"return ui.dialog{ open = false, title = "Hidden" }"#,
        &regular_viewport(),
    );
    assert!(matches!(
        render,
        RenderNode::Widget {
            widget_type: WidgetType::Empty,
            ..
        }
    ));
}

// =============================================================================
// Responsive resolution
// =============================================================================

#[test]
fn responsive_direction_resolves_against_viewport() {
    let lua = new_ui_lua();
    let code = r#"return ui.stack{
        direction = ui.responsive({ compact = "vertical", expanded = "horizontal" }),
        children = { ui.text{ text = "a" }, ui.text{ text = "b" } },
    }"#;

    // Compact viewport → vertical (VSplit).
    let (render_compact, _) = render_lua(&lua, code, &compact_viewport());
    assert!(matches!(render_compact, RenderNode::VSplit { .. }));

    // Expanded viewport → horizontal (HSplit).
    let (render_expanded, _) = render_lua(&lua, code, &expanded_viewport());
    assert!(matches!(render_expanded, RenderNode::HSplit { .. }));
}

#[test]
fn responsive_fallback_picks_smaller_class_first() {
    let lua = new_ui_lua();
    // Only `compact` and `expanded` defined — targeting `regular` should
    // fall back to `compact` per the adaptive spec.
    let code = r#"return ui.stack{
        direction = ui.responsive({ compact = "vertical", expanded = "horizontal" }),
        children = { ui.text{ text = "x" } },
    }"#;
    let (render, _) = render_lua(&lua, code, &regular_viewport());
    assert!(matches!(render, RenderNode::VSplit { .. }));
}

// =============================================================================
// Conditional rendering
// =============================================================================

#[test]
fn when_includes_child_only_when_condition_matches() {
    let lua = new_ui_lua();
    let code = r#"return ui.stack{
        direction = "horizontal",
        children = {
            ui.text{ text = "always" },
            ui.when("expanded", ui.text{ text = "only-expanded" }),
        },
    }"#;

    // compact → the `when` child is elided.
    let (compact_render, _) = render_lua(&lua, code, &compact_viewport());
    match compact_render {
        RenderNode::HSplit { children, .. } => assert_eq!(children.len(), 1),
        other => panic!("expected HSplit, got {other:?}"),
    }

    // expanded → both children render.
    let (expanded_render, _) = render_lua(&lua, code, &expanded_viewport());
    match expanded_render {
        RenderNode::HSplit { children, .. } => assert_eq!(children.len(), 2),
        other => panic!("expected HSplit, got {other:?}"),
    }
}

#[test]
fn hidden_drops_child_when_condition_matches() {
    let lua = new_ui_lua();
    let code = r#"return ui.stack{
        direction = "horizontal",
        children = {
            ui.text{ text = "always" },
            ui.hidden({ width = "compact" }, ui.text{ text = "hide-when-compact" }),
        },
    }"#;

    // compact → the `hidden` child is dropped.
    let (compact_render, _) = render_lua(&lua, code, &compact_viewport());
    match compact_render {
        RenderNode::HSplit { children, .. } => assert_eq!(children.len(), 1),
        other => panic!("expected HSplit, got {other:?}"),
    }

    // regular → the child renders because the condition does NOT match.
    let (regular_render, _) = render_lua(&lua, code, &regular_viewport());
    match regular_render {
        RenderNode::HSplit { children, .. } => assert_eq!(children.len(), 2),
        other => panic!("expected HSplit, got {other:?}"),
    }
}

// =============================================================================
// Viewport derivation
// =============================================================================

#[test]
fn derive_viewport_matches_default_thresholds() {
    assert_eq!(
        derive_viewport_from_terminal(40, 20, false).width_class,
        UiWidthClass::Compact
    );
    assert_eq!(
        derive_viewport_from_terminal(40, 20, false).height_class,
        UiHeightClass::Short
    );
    assert_eq!(
        derive_viewport_from_terminal(100, 30, false).width_class,
        UiWidthClass::Regular
    );
    assert_eq!(
        derive_viewport_from_terminal(130, 50, false).width_class,
        UiWidthClass::Expanded
    );
    assert_eq!(
        derive_viewport_from_terminal(130, 50, false).height_class,
        UiHeightClass::Tall
    );
}

#[test]
fn derive_viewport_pointer_coarse_when_mouse_supported() {
    let v = derive_viewport_from_terminal(100, 30, true);
    assert_eq!(v.pointer, UiPointer::Coarse);
}

// =============================================================================
// Codex iteration 1 — regression tests for F1 / F1b / F2 / F3
// =============================================================================

/// F1 — three rows sharing an action id keep three distinct envelopes in
/// the ActionTable, each with its own payload.
#[test]
fn action_table_preserves_per_row_payloads_for_shared_action_id() {
    let lua = new_ui_lua();
    let node = eval_node(
        &lua,
        r#"return {
            type = "list",
            children = {
                { type = "list_item", id = "row-a",
                  props = { action = ui.action("botster.session.select", { sessionUuid = "sess-a" }) },
                  slots = { title = { ui.text{ text = "A" } } } },
                { type = "list_item", id = "row-b",
                  props = { action = ui.action("botster.session.select", { sessionUuid = "sess-b" }) },
                  slots = { title = { ui.text{ text = "B" } } } },
                { type = "list_item", id = "row-c",
                  props = { action = ui.action("botster.session.select", { sessionUuid = "sess-c" }) },
                  slots = { title = { ui.text{ text = "C" } } } },
            },
        }"#,
    );
    let mut actions = ActionTable::new();
    let _render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let envelopes: Vec<_> = actions.by_action_id("botster.session.select").collect();
    assert_eq!(envelopes.len(), 3);
    let uuids: Vec<_> = envelopes
        .iter()
        .map(|e| {
            e.action
                .payload
                .get("sessionUuid")
                .and_then(|v| v.as_str())
                .expect("sessionUuid payload populated")
                .to_owned()
        })
        .collect();
    assert_eq!(uuids, vec!["sess-a", "sess-b", "sess-c"]);

    // Keys must be walk-order-unique. Explicit node.ids get `id:<id>`.
    assert!(actions.get("id:row-a").is_some());
    assert!(actions.get("id:row-b").is_some());
    assert!(actions.get("id:row-c").is_some());
}

/// F1b — a disabled action on a list_item row drops the legacy action
/// string (so activation is inert) but the full envelope remains
/// retrievable from the ActionTable.
#[test]
fn list_item_disabled_action_drops_legacy_string_but_keeps_envelope() {
    let lua = new_ui_lua();
    let node = eval_node(
        &lua,
        r#"
        local act = ui.action("botster.session.close.request", { sessionUuid = "sess-x" })
        act.disabled = true
        return {
            type = "list",
            children = {
                { type = "list_item", id = "row-x",
                  props = { action = act },
                  slots = { title = { ui.text{ text = "X" } } } },
            },
        }
        "#,
    );
    let mut actions = ActionTable::new();
    let render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let list_props = expect_list_props(&render);
    assert!(
        list_props.items[0].action.is_none(),
        "disabled list_item must not expose a legacy action string"
    );
    let envelope = actions
        .get("id:row-x")
        .expect("envelope recorded under node.id key");
    assert_eq!(envelope.action.disabled, Some(true));
    assert_eq!(
        envelope.action.payload.get("sessionUuid"),
        Some(&serde_json::json!("sess-x"))
    );
}

/// F2 — LayoutLua routing accepts a top-level `ui.list{...}` tree and
/// flows through the adapter (no fall-through to the legacy parser).
#[test]
fn routing_accepts_top_level_list() {
    // The Lua DSL does not expose a `ui.list` constructor in v1, so we
    // hand-build the node. The point of the test is that the adapter's
    // is_ui_node_type recognises `list` and the call_render path would
    // route to it. We test this directly via render_lua_ui_node, which
    // is what LayoutLua::call_render delegates to on a type-name match.
    let lua = new_ui_lua();
    let table: mlua::Table = lua
        .load(
            r#"return {
                type = "list",
                children = {
                    { type = "list_item",
                      props = { action = ui.action("botster.session.select", { sessionUuid = "only" }) },
                      slots = { title = { ui.text{ text = "Only" } } } },
                },
            }"#,
        )
        .eval()
        .expect("eval");
    let (render, _actions) =
        render_lua_ui_node(&lua, &table, &regular_viewport()).expect("adapter");
    assert!(matches!(
        render,
        RenderNode::Widget {
            widget_type: WidgetType::List,
            ..
        }
    ));
}

/// F2 — routing accepts a top-level `overlay` node.
#[test]
fn routing_accepts_top_level_overlay() {
    let lua = new_ui_lua();
    let table: mlua::Table = lua
        .load(
            r#"return {
                type = "overlay",
                children = { ui.text{ text = "Hello" } },
            }"#,
        )
        .eval()
        .expect("eval");
    let (render, _actions) =
        render_lua_ui_node(&lua, &table, &regular_viewport()).expect("adapter");
    assert!(matches!(render, RenderNode::Centered { .. }));
}

/// F3 — `ui.when` wrappers inside a slot's child array are filtered by
/// the slot reader, so the slot text matches the active viewport's
/// condition.
#[test]
fn list_item_title_slot_resolves_conditional_wrapper() {
    let lua = new_ui_lua();
    let source = r#"return {
        type = "list",
        children = {
            { type = "list_item",
              props = { action = ui.action("x") },
              slots = {
                  title = {
                      ui.when({ width = "expanded" }, ui.text{ text = "expanded-only" }),
                      ui.when({ width = "compact" },  ui.text{ text = "compact-only" }),
                  },
              } },
        },
    }"#;

    // Expanded viewport — expanded-only survives, compact-only is elided.
    let node_expanded = eval_node(&lua, source);
    let mut actions = ActionTable::new();
    let render_expanded =
        render_ui_node(&node_expanded, &expanded_viewport(), &mut actions).expect("render");
    let list_expanded = expect_list_props(&render_expanded);
    assert!(list_item_plain(&list_expanded.items[0].content).contains("expanded-only"));

    // Compact viewport — compact-only survives.
    let node_compact = eval_node(&lua, source);
    let mut actions2 = ActionTable::new();
    let render_compact =
        render_ui_node(&node_compact, &compact_viewport(), &mut actions2).expect("render");
    let list_compact = expect_list_props(&render_compact);
    assert!(list_item_plain(&list_compact.items[0].content).contains("compact-only"));
}

/// F3 — a `tree_item` subtitle slot also honors conditional wrappers.
#[test]
fn tree_item_subtitle_slot_resolves_conditional_wrapper() {
    let lua = new_ui_lua();
    let source = r#"return ui.tree{
        children = {
            ui.tree_item{
                id = "n1",
                title = { ui.text{ text = "N1" } },
                subtitle = {
                    ui.hidden({ width = "compact" }, ui.text{ text = "details" }),
                },
            },
        },
    }"#;

    // Regular viewport: hidden condition does NOT match, so the subtitle renders.
    let node = eval_node(&lua, source);
    let mut actions = ActionTable::new();
    let render = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
    let list = expect_list_props(&render);
    let subtitle = list.items[0]
        .secondary
        .as_ref()
        .expect("subtitle present at regular width");
    match subtitle {
        StyledContent::Plain(s) => assert_eq!(s, "details"),
        StyledContent::Styled(_) => panic!("subtitle expected Plain"),
    }

    // Compact viewport: hidden condition matches → subtitle elided.
    let node = eval_node(&lua, source);
    let mut actions2 = ActionTable::new();
    let render = render_ui_node(&node, &compact_viewport(), &mut actions2).expect("render");
    let list = expect_list_props(&render);
    assert!(
        list.items[0].secondary.is_none(),
        "compact viewport should elide hidden subtitle"
    );
}

/// Internal menu primitive renders through the adapter even though it
/// is not Lua-public in v1.
#[test]
fn internal_menu_renders_as_centered_list() {
    let lua = new_ui_lua();
    let table: mlua::Table = lua
        .load(
            r#"return {
                type = "menu",
                slots = {
                    items = {
                        { type = "menu_item",
                          props = { action = ui.action("botster.menu.item-a") },
                          slots = { title = { ui.text{ text = "Item A" } } } },
                        { type = "menu_item",
                          props = { action = ui.action("botster.menu.item-b") },
                          slots = { title = { ui.text{ text = "Item B" } } } },
                    },
                },
            }"#,
        )
        .eval()
        .expect("eval");
    let (render, actions) =
        render_lua_ui_node(&lua, &table, &regular_viewport()).expect("adapter");
    match render {
        RenderNode::Centered { child, .. } => match *child {
            RenderNode::Widget {
                widget_type: WidgetType::List,
                props: Some(WidgetProps::List(lp)),
                ..
            } => assert_eq!(lp.items.len(), 2),
            other => panic!("expected list inside centered overlay, got {other:?}"),
        },
        other => panic!("expected Centered overlay, got {other:?}"),
    }
    assert_eq!(actions.len(), 2);
}

// =============================================================================
// Helpers
// =============================================================================

fn expect_list_props(node: &RenderNode) -> &ListProps {
    match node {
        RenderNode::Widget {
            props: Some(WidgetProps::List(lp)),
            ..
        } => lp,
        RenderNode::Widget {
            widget_type: WidgetType::List,
            ..
        } => panic!("widget has no list props"),
        other => panic!("expected List widget, got {other:?}"),
    }
}

fn list_item_plain(content: &StyledContent) -> String {
    match content {
        StyledContent::Plain(s) => s.clone(),
        StyledContent::Styled(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
    }
}

fn paragraph_text_contains(node: &RenderNode, needle: &str) -> bool {
    match node {
        RenderNode::Widget {
            props: Some(WidgetProps::Paragraph(ParagraphProps { lines, .. })),
            ..
        } => lines.iter().any(|line| match line {
            StyledContent::Plain(s) => s.contains(needle),
            StyledContent::Styled(spans) => spans.iter().any(|sp| sp.text.contains(needle)),
        }),
        _ => false,
    }
}

