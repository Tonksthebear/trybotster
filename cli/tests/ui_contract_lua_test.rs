//! Integration tests for the cross-client UI DSL.
//!
//! These tests exercise the Lua constructors end-to-end: they register the
//! `ui` table in a fresh `mlua::Lua` VM, run Lua code, and check that the
//! resulting JSON matches the wire format defined by
//! `docs/specs/cross-client-ui-primitives.md`.
//!
//! Phase B (TUI adapter) and Phase C (web renderer) will deserialize the same
//! JSON into their respective render trees. Any drift between this test suite
//! and the specs means a renderer would silently misrender.
//!
//! Run with: `cargo test --test ui_contract_lua_test` (also runnable via
//! `./test.sh --integration` — plain filter args to `./test.sh` filter by
//! test name, not test-file name).

#![expect(
    clippy::unwrap_used,
    clippy::needless_borrows_for_generic_args,
    clippy::redundant_closure_for_method_calls,
    reason = "test-code brevity: these lints flag patterns intentionally used in tests"
)]

use botster::ui_contract::lua::register;
use botster::ui_contract::node::{UiChild, UiConditional, UiNode};
use botster::ui_contract::{
    BadgeProps, ButtonProps, DialogProps, EmptyStateProps, IconButtonProps, IconProps, InlineProps,
    PanelProps, ScrollAreaProps, StackProps, StatusDotProps, TextProps, TreeItemProps,
    UiWidthClass,
};
use mlua::{Lua, LuaSerdeExt, Value};
use serde_json::{json, Value as JsonValue};

fn new_ui_lua() -> Lua {
    let lua = Lua::new();
    register(&lua).expect("register ui primitives");
    lua
}

fn eval_to_json(lua: &Lua, code: &str) -> JsonValue {
    let value: Value = lua.load(code).eval().expect("Lua eval failed");
    lua.from_value(value)
        .expect("Lua -> JSON conversion failed")
}

fn eval_to_node(lua: &Lua, code: &str) -> UiNode {
    let value: Value = lua.load(code).eval().expect("Lua eval failed");
    lua.from_value(value)
        .expect("Lua -> UiNode conversion failed")
}

// =============================================================================
// Layout primitives
// =============================================================================

#[test]
fn stack_wire_shape_and_typed_props_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.stack{ direction = "vertical", gap = "2" }"#,
    );
    assert_eq!(node.node_type, "stack");
    let props: StackProps = serde_json::from_value(serde_json::Value::Object(node.props))
        .expect("StackProps deserialize");
    // Scalar direction + gap round-trips cleanly; padding is NOT in current shared contract.
    assert_eq!(
        serde_json::to_value(&props).expect("re-serialize"),
        json!({ "direction": "vertical", "gap": "2" })
    );
}

#[test]
fn stack_rejects_missing_direction_at_lua_layer() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.stack{ gap = "2" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("direction"), "got {err}");
}

#[test]
fn inline_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.inline{ gap = "1", wrap = true, justify = "start" }"#,
    );
    let props: InlineProps = serde_json::from_value(serde_json::Value::Object(node.props))
        .expect("InlineProps deserialize");
    assert_eq!(
        serde_json::to_value(&props).expect("re-serialize"),
        json!({ "gap": "1", "wrap": true, "justify": "start" })
    );
}

#[test]
fn panel_typed_round_trip_with_interaction_density() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.panel{ title = "Preview", tone = "muted", border = true,
                            interaction_density = "comfortable" }"#,
    );
    let props: PanelProps = serde_json::from_value(serde_json::Value::Object(node.props))
        .expect("PanelProps deserialize");
    let re = serde_json::to_value(&props).expect("re-serialize");
    assert_eq!(
        re,
        json!({
            "title": "Preview",
            "tone": "muted",
            "border": true,
            "interactionDensity": "comfortable"
        })
    );
}

#[test]
fn panel_rejects_web_only_padding_and_radius_at_construction() {
    // Lua layer REJECTS web-only fields before they reach the wire — the prop
    // allowlist is the enforcement point, not typed Rust deserialization.
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.panel{ padding = "4", title = "x" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("unknown prop"), "got {err}");
    assert!(err.to_string().contains("padding"), "got {err}");

    let err = lua
        .load(r#"return ui.panel{ radius = "md", title = "x" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("unknown prop"), "got {err}");
    assert!(err.to_string().contains("radius"), "got {err}");
}

#[test]
fn button_rejects_web_only_leading_icon_and_disabled() {
    let lua = new_ui_lua();
    // Both snake_case and camelCase forms of the web-only name are rejected.
    for leading in ["leading_icon", "leadingIcon"] {
        let code = format!(
            r#"return ui.button{{ label = "x", action = ui.action("a"), {leading} = "check" }}"#
        );
        let err = lua.load(&code).eval::<Value>().unwrap_err();
        assert!(err.to_string().contains("unknown prop"), "got {err}");
    }
    let err = lua
        .load(r#"return ui.button{ label = "x", action = ui.action("a"), disabled = true }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("disabled"), "got {err}");
}

#[test]
fn stack_rejects_unknown_prop() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.stack{ direction = "vertical", foo = "bar" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("unknown prop"), "got {err}");
    assert!(err.to_string().contains("foo"), "got {err}");
}

#[test]
fn stack_rejects_web_only_padding() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.stack{ direction = "vertical", padding = "4" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("padding"), "got {err}");
}

#[test]
fn icon_button_rejects_disabled() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.icon_button{ icon = "x", label = "x", action = ui.action("a"), disabled = true }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("disabled"), "got {err}");
}

#[test]
fn scroll_area_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(&lua, r#"return ui.scroll_area{ axis = "y" }"#);
    let props: ScrollAreaProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    assert_eq!(
        serde_json::to_value(&props).expect("re-serialize"),
        json!({ "axis": "y" })
    );
}

// =============================================================================
// Content primitives
// =============================================================================

#[test]
fn text_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.text{ text = "Hello", tone = "accent", size = "sm", weight = "medium",
                           monospace = true, italic = false, truncate = true }"#,
    );
    let props: TextProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    let re = serde_json::to_value(&props).expect("re-serialize");
    assert_eq!(
        re,
        json!({
            "text": "Hello",
            "tone": "accent",
            "size": "sm",
            "weight": "medium",
            "monospace": true,
            "italic": false,
            "truncate": true
        })
    );
}

#[test]
fn icon_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.icon{ name = "workspace", size = "sm", tone = "muted", label = "Workspaces" }"#,
    );
    let props: IconProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    assert_eq!(props.name, "workspace");
    assert_eq!(props.label.as_deref(), Some("Workspaces"));
}

#[test]
fn badge_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(&lua, r#"return ui.badge{ text = "3", tone = "warning" }"#);
    let props: BadgeProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    assert_eq!(props.text, "3");
}

#[test]
fn status_dot_typed_round_trip() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.status_dot{ state = "active", label = "Running" }"#,
    );
    let props: StatusDotProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    assert_eq!(props.label.as_deref(), Some("Running"));
}

#[test]
fn empty_state_typed_round_trip_with_snake_case_primary_action() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"
            return ui.empty_state{
                title = "No sessions yet",
                description = "Spawn an agent to get started.",
                icon = "sparkle",
                primary_action = ui.action("botster.session.create.request"),
            }
        "#,
    );
    let props: EmptyStateProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    let action = props.primary_action.expect("primary_action present");
    assert_eq!(action.id, "botster.session.create.request");
}

// =============================================================================
// Action primitives
// =============================================================================

#[test]
fn button_wire_shape_uses_icon_not_leading_icon() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"
            return ui.button{
                label = "Save",
                action = ui.action("botster.workspace.save", { workspaceId = "ws-1" }),
                variant = "solid",
                tone = "accent",
                icon = "check",
            }
        "#,
    );
    let props: ButtonProps = serde_json::from_value(serde_json::Value::Object(node.props))
        .expect("ButtonProps deserialize");
    assert_eq!(props.icon.as_deref(), Some("check"));
    let re = serde_json::to_value(&props).expect("re-serialize");
    assert!(
        re.get("leadingIcon").is_none(),
        "Button wire must not carry leadingIcon"
    );
    assert!(
        re.get("disabled").is_none(),
        "Button wire must not carry disabled"
    );
}

#[test]
fn icon_button_typed_round_trip_no_disabled() {
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"
            return ui.icon_button{
                icon = "close",
                label = "Close session",
                action = ui.action("botster.session.close.request", { sessionUuid = "sess-1" }),
            }
        "#,
    );
    let props: IconButtonProps =
        serde_json::from_value(serde_json::Value::Object(node.props)).expect("deserialize");
    let re = serde_json::to_value(&props).expect("re-serialize");
    assert!(
        re.get("disabled").is_none(),
        "IconButton wire must not carry disabled"
    );
    assert_eq!(props.label, "Close session");
}

// =============================================================================
// Navigation primitives
// =============================================================================

#[test]
fn tree_has_no_shared_props_and_accepts_children() {
    // Tree carries NO shared props in current — empty `props` map on the wire.
    // Deliberately no TreeProps struct exists.
    let lua = new_ui_lua();
    let node = eval_to_node(
        &lua,
        r#"return ui.tree{ children = { ui.tree_item{ id="x", title = { ui.text{ text = "X" } } } } }"#,
    );
    assert!(
        node.props.is_empty(),
        "tree must not emit props: {:?}",
        node.props
    );
    assert_eq!(node.children.len(), 1);
}

#[test]
fn tree_item_with_all_optional_slots() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.tree_item{
                id = "ws-1",
                selected = true,
                expanded = true,
                notification = true,
                action = ui.action("botster.workspace.toggle", { workspaceId = "ws-1" }),
                slots = {
                    title    = { ui.text{ text = "Workspace" } },
                    subtitle = { ui.text{ text = "3 sessions" } },
                    start    = { ui.status_dot{ state = "active" } },
                    end_     = { ui.badge{ text = "3" } },
                    children = { ui.tree_item{ id = "sess-1", title = { ui.text{ text = "sess" } } } },
                },
            }
        "#,
    );
    assert_eq!(v.get("type").and_then(|v| v.as_str()), Some("tree_item"));
    assert_eq!(v.get("id").and_then(|v| v.as_str()), Some("ws-1"));
    let slots = v.get("slots").expect("slots present");
    for expected in ["title", "subtitle", "start", "end", "children"] {
        assert!(
            slots.get(expected).is_some(),
            "slot {expected} missing: {slots:?}"
        );
    }
    let props: TreeItemProps =
        serde_json::from_value(v.get("props").unwrap().clone()).expect("TreeItemProps deserialize");
    assert_eq!(props.selected, Some(true));
    assert_eq!(props.expanded, Some(true));
}

#[test]
fn tree_item_without_title_slot_raises_error() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.tree_item{ id = "x" }"#)
        .eval::<Value>()
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ui.tree_item"), "got {err}");
    assert!(msg.contains("title"), "error must mention `title`: {err}");
}

#[test]
fn tree_item_rejects_unknown_slot_key() {
    let lua = new_ui_lua();
    let err = lua
        .load(
            r#"return ui.tree_item{
                id = "x",
                slots = {
                    title = { ui.text{ text = "t" } },
                    footr = { ui.text{ text = "typo" } },
                },
            }"#,
        )
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("footr"), "got {err}");
}

#[test]
fn primitive_without_slots_rejects_any_slot_input() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.stack{ direction = "vertical", slots = { title = { ui.text{ text="x" } } } }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("unknown slot"), "got {err}");
}

// =============================================================================
// Dialog (internal)
// =============================================================================

#[test]
fn dialog_wire_shape_with_hoisted_slots() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.dialog{
                open = true,
                title = "Rename Workspace",
                presentation = "sheet",
                body = { ui.text{ text = "Enter a new name" } },
                footer = { ui.button{ label = "Save", action = ui.action("botster.workspace.rename.commit") } },
            }
        "#,
    );
    let props: DialogProps =
        serde_json::from_value(v.get("props").unwrap().clone()).expect("DialogProps deserialize");
    assert!(props.open);
    assert_eq!(props.title, "Rename Workspace");
    let slots = v.get("slots").expect("slots");
    assert!(slots.get("body").is_some());
    assert!(slots.get("footer").is_some());
    // body/footer must not leak into props per spec.
    let props_raw = v.get("props").unwrap();
    assert!(props_raw.get("body").is_none());
    assert!(props_raw.get("footer").is_none());
}

#[test]
fn dialog_rejects_unknown_slot() {
    let lua = new_ui_lua();
    let err = lua
        .load(
            r#"return ui.dialog{
                open = true,
                title = "x",
                slots = { header = { ui.text{ text = "nope" } } },
            }"#,
        )
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("header"), "got {err}");
}

#[test]
fn dialog_requires_open_boolean() {
    let lua = new_ui_lua();
    let err = lua
        .load(r#"return ui.dialog{ title = "x" }"#)
        .eval::<Value>()
        .unwrap_err();
    assert!(err.to_string().contains("open"), "got {err}");
}

// =============================================================================
// Responsive + conditional
// =============================================================================

#[test]
fn responsive_width_shorthand_matches_spec() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"return ui.responsive({ compact = "vertical", expanded = "horizontal" })"#,
    );
    assert_eq!(
        v,
        json!({
            "$kind": "responsive",
            "width": { "compact": "vertical", "expanded": "horizontal" }
        })
    );
}

#[test]
fn responsive_explicit_form_allows_both_dimensions() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.responsive({
                width  = { compact = "comfortable", expanded = "compact" },
                height = { short = "compact", tall = "comfortable" },
            })
        "#,
    );
    assert_eq!(
        v,
        json!({
            "$kind": "responsive",
            "width":  { "compact": "comfortable", "expanded": "compact" },
            "height": { "short":   "compact",     "tall":     "comfortable" }
        })
    );
}

#[test]
fn responsive_value_embeds_inside_primitive_prop() {
    // Responsive values at any prop-value position must survive unmodified so
    // renderers can detect the $kind discriminator anywhere in the tree.
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.stack{
                direction = ui.responsive({ compact = "vertical", expanded = "horizontal" }),
                gap = "2",
            }
        "#,
    );
    assert_eq!(
        v,
        json!({
            "type": "stack",
            "props": {
                "direction": {
                    "$kind": "responsive",
                    "width": { "compact": "vertical", "expanded": "horizontal" }
                },
                "gap": "2"
            }
        })
    );
}

#[test]
fn when_hidden_wrappers_accepted_in_children_position() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.stack{
                direction = "vertical",
                children = {
                    ui.when("expanded", ui.text{ text = "Desktop only" }),
                    ui.hidden({ width = "compact" }, ui.badge{ text = "hidden-on-compact" }),
                },
            }
        "#,
    );
    assert_eq!(
        v,
        json!({
            "type": "stack",
            "props": { "direction": "vertical" },
            "children": [
                {
                    "$kind": "when",
                    "condition": { "width": "expanded" },
                    "node": { "type": "text", "props": { "text": "Desktop only" } }
                },
                {
                    "$kind": "hidden",
                    "condition": { "width": "compact" },
                    "node": { "type": "badge", "props": { "text": "hidden-on-compact" } }
                }
            ]
        })
    );
}

#[test]
fn children_roundtrip_through_typed_uinode_with_wrappers() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"
            return ui.stack{
                direction = "vertical",
                children = {
                    ui.text{ text = "first" },
                    ui.when("expanded", ui.text{ text = "second-expanded" }),
                },
            }
        "#,
    );
    let typed: UiNode = serde_json::from_value(v).expect("typed deserialize");
    assert_eq!(typed.node_type, "stack");
    assert_eq!(typed.children.len(), 2);
    assert!(matches!(typed.children[0], UiChild::Node(_)));
    match &typed.children[1] {
        UiChild::Conditional(UiConditional::When { condition, node }) => {
            assert_eq!(condition.width, Some(UiWidthClass::Expanded));
            assert_eq!(node.node_type, "text");
        }
        other => panic!("expected a `when` wrapper, got {other:?}"),
    }
}

// =============================================================================
// Ownership + scope
// =============================================================================

#[test]
fn menu_and_menu_item_not_lua_public() {
    let lua = new_ui_lua();
    let menu: Value = lua.load("return ui.menu").eval().expect("eval");
    assert!(matches!(menu, Value::Nil));
    let menu_item: Value = lua.load("return ui.menu_item").eval().expect("eval");
    assert!(matches!(menu_item, Value::Nil));
}

#[test]
fn text_input_checkbox_toggle_select_not_lua_public() {
    // These primitives are in the broader cross-client shared vocabulary but
    // intentionally deferred from the current Lua-public inventory per
    // `phase one web ui composites stay internal while Lua public contract stops at primitives.md`.
    let lua = new_ui_lua();
    for name in ["text_input", "checkbox", "toggle", "select"] {
        let v: Value = lua.load(&format!("return ui.{name}")).eval().expect("eval");
        assert!(
            matches!(v, Value::Nil),
            "ui.{name} must not be exposed in current"
        );
    }
}

#[test]
fn controlled_uncontrolled_rule_id_preserved_outside_props() {
    let lua = new_ui_lua();
    let v = eval_to_json(
        &lua,
        r#"return ui.tree_item{ id = "sess-1", selected = true, title = { ui.text{ text = "x" } } }"#,
    );
    assert_eq!(v.get("id").and_then(|v| v.as_str()), Some("sess-1"));
    let props = v.get("props").expect("props");
    assert!(props.get("id").is_none(), "id must not appear under props");
    assert_eq!(props.get("selected").and_then(|v| v.as_bool()), Some(true));
}
