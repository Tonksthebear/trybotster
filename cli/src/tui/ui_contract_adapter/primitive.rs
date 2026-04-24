//! Primitive → [`RenderNode`] mapping for the TUI adapter.
//!
//! This module is the meat of Phase B: it translates a [`UiNodeV1`] tree
//! (produced by Phase A's Lua DSL) into the existing TUI render tree so
//! ratatui can draw it unchanged.
//!
//! # Mapping table
//!
//! | Shared primitive  | TUI realization                                         |
//! |-------------------|---------------------------------------------------------|
//! | `stack` (horiz.)  | [`RenderNode::HSplit`]                                  |
//! | `stack` (vert.)   | [`RenderNode::VSplit`]                                  |
//! | `inline`          | [`RenderNode::HSplit`]                                  |
//! | `panel`           | Bordered [`BlockConfig`] wrapping a vertical stack      |
//! | `scroll_area`     | Child pass-through (see "Known limits" below)           |
//! | `list`            | [`WidgetType::List`] with [`ListProps`]                 |
//! | `list_item`       | one [`ListItemProps`] row, slot-merged                  |
//! | `tree`            | [`WidgetType::List`] with indented rows                 |
//! | `tree_item`       | indented [`ListItemProps`], recurses on `children` slot |
//! | `text`            | [`WidgetType::Paragraph`] with a single styled line     |
//! | `icon`            | [`WidgetType::Paragraph`] with one colored glyph span   |
//! | `badge`           | [`WidgetType::Paragraph`] with bracketed span           |
//! | `status_dot`      | [`WidgetType::Paragraph`] one colored bullet            |
//! | `empty_state`     | [`RenderNode::Centered`] of a titled vertical stack     |
//! | `button`          | single-item [`WidgetType::List`] w/ action string       |
//! | `icon_button`     | single-item [`WidgetType::List`] w/ icon+label          |
//! | `dialog`          | [`RenderNode::Centered`] overlay wrapping body/footer   |
//! | `menu` (internal) | [`RenderNode::Centered`] list of `menu_item`s           |
//!
//! # Known limits
//!
//! - `scroll_area`: ratatui's current TUI has no dedicated scroll widget
//!   exposed through [`WidgetType`]. The adapter renders the children
//!   directly (no viewport clipping) and relies on the author / layout
//!   above to constrain height. Authors targeting the TUI should treat
//!   `scroll_area` as advisory until a dedicated widget lands.
//! - `inline.wrap` is ignored (the TUI layout engine handles overflow
//!   via constraints, not reflow).
//! - `panel` border rendering uses the existing `BlockConfig` hook on the
//!   innermost widget: when the panel contains one child, the block is
//!   attached to that child; when it contains multiple children, the
//!   panel title is rendered as a header paragraph above a vertical
//!   stack. This keeps the adapter purely additive over the existing
//!   render tree — no new [`RenderNode`] variants.
//!
//! [`BlockConfig`]: crate::tui::render_tree::BlockConfig
//! [`ListProps`]: crate::tui::render_tree::ListProps
//! [`ListItemProps`]: crate::tui::render_tree::ListItemProps
//! [`WidgetType`]: crate::tui::render_tree::WidgetType

// Rust guideline compliant 2026-04-18

#![expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "UiViewportV1 is Copy but we pass by reference deliberately — it reads as 'the current viewport context this pass is rendering against', and the consistency across 30+ adapter signatures outweighs the trivial copy optimisation"
)]

use anyhow::{anyhow, Result};
use ratatui::layout::Constraint;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::tui::entity_stores::TuiEntityStores;
use crate::tui::render_tree::{
    BlockConfig, BorderStyle, ListItemProps, ListProps, ParagraphAlignment, ParagraphProps,
    RenderNode, SpanStyle, StyledContent, StyledSpan, WidgetProps, WidgetType,
};
use crate::ui_contract::node::{UiActionV1, UiChildV1, UiNodeV1};
use crate::ui_contract::props::{
    BadgePropsV1, ButtonPropsV1, ConnectionCodePropsV1, DialogPropsV1, EmptyStatePropsV1,
    HubRecoveryStatePropsV1, IconButtonPropsV1, IconPropsV1, NewSessionButtonPropsV1, PanelPropsV1,
    SessionListPropsV1, SessionRowPropsV1, SpawnTargetListPropsV1, StackPropsV1, StatusDotPropsV1,
    TextPropsV1, TreeItemPropsV1, WorkspaceListPropsV1, WorktreeListPropsV1,
};
use crate::ui_contract::tokens::{UiSessionListGrouping, UiStackDirection, UiSurfaceDensity};
use crate::ui_contract::viewport::UiViewportV1;

use super::action::ActionTable;
use super::responsive::{filter_children, resolve_props};
use super::style::{
    badge_tone_color, button_tone_color, single_span, status_dot_color, text_span_style,
    tone_color, BUTTON_HIGHLIGHT_SYMBOL,
};

/// Names of every [`UiNodeV1`] primitive the adapter understands.
///
/// Used by [`is_ui_node_type`] so callers that hold a Lua table can
/// decide whether to route it through the adapter or the legacy
/// `RenderNode::from_lua_table` path.
///
/// Every entry here MUST have a matching branch in [`render_ui_node`];
/// the two lists are verified to stay in sync by
/// `is_ui_node_type_recognises_all_primitives` + the fuzz-like
/// coverage sweep in the unit tests below.
const UI_NODE_TYPE_NAMES: &[&str] = &[
    // Layout
    "stack",
    "inline",
    "panel",
    "scroll_area",
    "overlay",
    // Content
    "text",
    "icon",
    "badge",
    "status_dot",
    "empty_state",
    // Action
    "button",
    "icon_button",
    // Collections
    "list",
    "list_item",
    "tree",
    "tree_item",
    // Internal / experimental — recognised so renderers can consume
    // programmatically-constructed nodes even though the Lua DSL does
    // not expose them as constructors in v1.
    "dialog",
    "menu",
    "menu_item",
    // Wire protocol v2 composites — data-driven, read from TuiEntityStores.
    "session_list",
    "workspace_list",
    "spawn_target_list",
    "worktree_list",
    "session_row",
    "hub_recovery_state",
    "connection_code",
    "new_session_button",
];

/// Default proportion of overlay width for `dialog` / `menu`.
///
/// Chosen to mirror the proportions used by existing `TuiRunner` overlays.
const OVERLAY_WIDTH_PCT: u16 = 70;
/// Default proportion of overlay height for `dialog` / `menu`.
const OVERLAY_HEIGHT_PCT: u16 = 60;
/// Percentage of the centred area occupied by `empty_state`.
const EMPTY_STATE_WIDTH_PCT: u16 = 80;
/// Percentage of the centred area occupied by `empty_state`.
const EMPTY_STATE_HEIGHT_PCT: u16 = 70;

/// Returns `true` if `type_name` names a Phase A Lua-public primitive.
///
/// Used as a cheap discriminator so the TUI runner can route a Lua table
/// either through this adapter or through the legacy
/// [`RenderNode::from_lua_table`] path, without a flag day.
#[must_use]
pub fn is_ui_node_type(type_name: &str) -> bool {
    UI_NODE_TYPE_NAMES.contains(&type_name)
}

/// Render a single [`UiNodeV1`] into a [`RenderNode`], consuming any
/// `$kind = "responsive"` props and populating `actions` with every
/// [`UiActionV1`] encountered along the way.
///
/// Backward-compat wrapper around [`render_ui_node_with_stores`] for v1
/// callers that have no [`TuiEntityStores`] context. Wire protocol v2
/// composites (`session_list`, `workspace_list`, …) render their empty
/// state when called this way — pass stores explicitly via
/// [`render_ui_node_with_stores`] to render real content.
///
/// # Errors
///
/// Returns an error if the node has an unknown primitive type, or if its
/// `props` do not deserialise into the expected Phase A props struct.
pub fn render_ui_node(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    render_ui_node_with_stores(node, viewport, actions, None)
}

/// Render a single [`UiNodeV1`] into a [`RenderNode`] with optional
/// access to the v2 entity stores. The wire protocol v2 composites
/// (`session_list`, `workspace_list`, …) read their data from these
/// stores; existing primitives ignore them.
///
/// # Errors
///
/// Returns an error if the node has an unknown primitive type, or if its
/// `props` do not deserialise into the expected Phase A props struct.
pub fn render_ui_node_with_stores(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    match node.node_type.as_str() {
        "stack" => render_stack(node, viewport, actions, stores),
        "inline" => render_inline(node, viewport, actions, stores),
        "panel" => render_panel(node, viewport, actions, stores),
        "scroll_area" => render_scroll_area(node, viewport, actions, stores),
        "overlay" => render_overlay(node, viewport, actions, stores),
        "text" => render_text(node, viewport),
        "icon" => render_icon(node, viewport),
        "badge" => render_badge(node, viewport),
        "status_dot" => render_status_dot(node, viewport),
        "empty_state" => render_empty_state(node, viewport, actions),
        "button" => render_button(node, viewport, actions),
        "icon_button" => render_icon_button(node, viewport, actions),
        "list" => render_list(node, viewport, actions),
        "list_item" => render_standalone_list_item(node, viewport, actions),
        // `tree` / `tree_item` use flatten_tree_item which extracts text
        // from slots; no v2 composite recursion through them today.
        "tree" => render_tree(node, viewport, actions),
        "tree_item" => render_standalone_tree_item(node, viewport, actions),
        "dialog" => render_dialog(node, viewport, actions, stores),
        "menu" => render_menu(node, viewport, actions, stores),
        "menu_item" => render_standalone_menu_item(node, viewport, actions, stores),
        // Wire protocol v2 composites.
        "session_list" => render_session_list(node, viewport, actions, stores),
        "workspace_list" => render_workspace_list(node, viewport, actions, stores),
        "spawn_target_list" => render_spawn_target_list(node, viewport, actions, stores),
        "worktree_list" => render_worktree_list(node, viewport, actions, stores),
        "session_row" => render_session_row(node, viewport, actions, stores),
        "hub_recovery_state" => render_hub_recovery_state(node, viewport, actions, stores),
        "connection_code" => render_connection_code(node, viewport, actions, stores),
        "new_session_button" => render_new_session_button(node, viewport, actions),
        other => Err(anyhow!("ui_contract_adapter: unknown primitive `{other}`")),
    }
}

// =============================================================================
// Layout primitives
// =============================================================================

fn render_stack(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<StackPropsV1>(&node.props, viewport, "stack")?;
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions, stores)?;
    let constraints = default_min_zero_constraints(rendered.len());
    match resolve_stack_direction(&props) {
        UiStackDirection::Horizontal => Ok(RenderNode::HSplit {
            constraints,
            children: rendered,
        }),
        UiStackDirection::Vertical => Ok(RenderNode::VSplit {
            constraints,
            children: rendered,
        }),
    }
}

fn render_inline(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions, stores)?;
    let constraints = default_min_zero_constraints(rendered.len());
    Ok(RenderNode::HSplit {
        constraints,
        children: rendered,
    })
}

fn render_panel(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<PanelPropsV1>(&node.props, viewport, "panel")?;
    let children = filter_children(&node.children, viewport);
    let block = build_panel_block(&props);

    match children.len() {
        0 => {
            // Empty panel — render the block on an Empty widget so borders
            // / title still draw.
            Ok(RenderNode::Widget {
                widget_type: WidgetType::Empty,
                id: node.id.clone(),
                block,
                custom_lines: None,
                props: None,
            })
        }
        1 => {
            // Single child — attach the block to it directly so the
            // border wraps the child's content with no extra chrome.
            let inner = render_ui_node_with_stores(&children[0], viewport, actions, stores)?;
            Ok(attach_block_if_widget(inner, block, node.id.clone()))
        }
        _ => {
            // Multiple children — borders cannot wrap a split node without
            // introducing a new RenderNode variant, so we render the
            // title as a separator row and stack the children below it.
            // This is a deliberate, documented fidelity trade: see this
            // module's doc comment under "Known limits".
            let mut rendered = Vec::with_capacity(children.len() + 1);
            let mut constraints: Vec<Constraint> = Vec::with_capacity(children.len() + 1);
            if props.title.is_some() || matches!(props.border, Some(true)) {
                rendered.push(panel_header_widget(&props, node.id.as_deref()));
                constraints.push(Constraint::Length(1));
            }
            for child in &children {
                rendered.push(render_ui_node_with_stores(child, viewport, actions, stores)?);
                constraints.push(Constraint::Min(0));
            }
            Ok(RenderNode::VSplit {
                constraints,
                children: rendered,
            })
        }
    }
}

fn render_scroll_area(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    // The TUI has no dedicated scroll widget today — pass the children
    // through in a vertical stack so the layout constraints inherited
    // from the parent still bound the area. Documented under "Known
    // limits" in this module's doc comment.
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions, stores)?;
    let constraints = default_min_zero_constraints(rendered.len());
    if rendered.len() == 1 {
        // Zero-overhead pass-through for the common case.
        return Ok(rendered.into_iter().next().expect("len == 1"));
    }
    Ok(RenderNode::VSplit {
        constraints,
        children: rendered,
    })
}

fn render_overlay(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    // `overlay` is not Lua-public in Phase A but the mapping table in the
    // cross-client spec names it explicitly: wrap the first non-null
    // child in a centred area.
    let children = filter_children(&node.children, viewport);
    let first = children
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("ui_contract_adapter: overlay requires at least one child"))?;
    let child = render_ui_node_with_stores(&first, viewport, actions, stores)?;
    Ok(RenderNode::Centered {
        width_pct: OVERLAY_WIDTH_PCT,
        height_pct: OVERLAY_HEIGHT_PCT,
        child: Box::new(child),
    })
}

// =============================================================================
// Content primitives
// =============================================================================

fn render_text(node: &UiNodeV1, viewport: &UiViewportV1) -> Result<RenderNode> {
    let props = decode_props::<TextPropsV1>(&node.props, viewport, "text")?;
    let style = text_span_style(props.tone, props.weight, props.italic.unwrap_or(false));
    let content = single_span(&props.text, style);
    Ok(RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment: ParagraphAlignment::Left,
            wrap: !props.truncate.unwrap_or(false),
        })),
    })
}

fn render_icon(node: &UiNodeV1, viewport: &UiViewportV1) -> Result<RenderNode> {
    let props = decode_props::<IconPropsV1>(&node.props, viewport, "icon")?;
    // The TUI has no icon font — render the icon id as a short label so
    // the Lua author's intent still communicates. Tone drives colour.
    let style = SpanStyle {
        fg: props.tone.and_then(tone_color),
        ..SpanStyle::default()
    };
    let glyph = icon_glyph_for(&props.name);
    let content = single_span(glyph, style);
    Ok(RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment: ParagraphAlignment::Left,
            wrap: false,
        })),
    })
}

fn render_badge(node: &UiNodeV1, viewport: &UiViewportV1) -> Result<RenderNode> {
    let props = decode_props::<BadgePropsV1>(&node.props, viewport, "badge")?;
    let style = SpanStyle {
        fg: props.tone.and_then(badge_tone_color),
        bold: true,
        ..SpanStyle::default()
    };
    let content = StyledContent::Styled(vec![StyledSpan {
        text: format!("[{}]", props.text),
        style,
    }]);
    Ok(RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment: ParagraphAlignment::Left,
            wrap: false,
        })),
    })
}

fn render_status_dot(node: &UiNodeV1, viewport: &UiViewportV1) -> Result<RenderNode> {
    let props = decode_props::<StatusDotPropsV1>(&node.props, viewport, "status_dot")?;
    let (glyph, color) = status_dot_color(props.state);
    let style = SpanStyle {
        fg: color,
        ..SpanStyle::default()
    };
    let content = single_span(glyph, style);
    Ok(RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment: ParagraphAlignment::Left,
            wrap: false,
        })),
    })
}

fn render_empty_state(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let props = decode_props::<EmptyStatePropsV1>(&node.props, viewport, "empty_state")?;

    let mut rows: Vec<RenderNode> = Vec::with_capacity(4);
    let mut constraints: Vec<Constraint> = Vec::with_capacity(4);

    if let Some(icon_name) = &props.icon {
        rows.push(paragraph_widget_line(
            &icon_glyph_for(icon_name),
            SpanStyle {
                fg: Some(crate::tui::render_tree::SpanColor::Cyan),
                ..SpanStyle::default()
            },
            ParagraphAlignment::Center,
        ));
        constraints.push(Constraint::Length(1));
    }

    rows.push(paragraph_widget_line(
        &props.title,
        SpanStyle {
            bold: true,
            ..SpanStyle::default()
        },
        ParagraphAlignment::Center,
    ));
    constraints.push(Constraint::Length(1));

    if let Some(desc) = &props.description {
        rows.push(paragraph_widget_line(
            desc,
            SpanStyle {
                fg: Some(crate::tui::render_tree::SpanColor::Gray),
                ..SpanStyle::default()
            },
            ParagraphAlignment::Center,
        ));
        constraints.push(Constraint::Min(1));
    }

    if let Some(action) = props.primary_action {
        rows.push(button_widget(&action.id, &action, None, None));
        constraints.push(Constraint::Length(1));
        actions.insert(node.id.as_deref(), action);
    }

    Ok(RenderNode::Centered {
        width_pct: EMPTY_STATE_WIDTH_PCT,
        height_pct: EMPTY_STATE_HEIGHT_PCT,
        child: Box::new(RenderNode::VSplit {
            constraints,
            children: rows,
        }),
    })
}

// =============================================================================
// Action primitives
// =============================================================================

fn render_button(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let props = decode_props::<ButtonPropsV1>(&node.props, viewport, "button")?;
    let icon_prefix = props.icon.as_deref();
    let tone = props.tone.and_then(button_tone_color);
    let render = button_widget(&props.label, &props.action, icon_prefix, tone);
    actions.insert(node.id.as_deref(), props.action);
    Ok(apply_id(render, node.id.clone()))
}

fn render_icon_button(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let props = decode_props::<IconButtonPropsV1>(&node.props, viewport, "icon_button")?;
    let glyph = icon_glyph_for(&props.icon);
    let label = format!("{glyph} {}", props.label);
    let tone = props.tone.and_then(button_tone_color);
    let render = button_widget(&label, &props.action, None, tone);
    actions.insert(node.id.as_deref(), props.action);
    Ok(apply_id(render, node.id.clone()))
}

// =============================================================================
// Collection primitives
// =============================================================================

fn render_list(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let children = filter_children(&node.children, viewport);
    let mut items: Vec<ListItemProps> = Vec::with_capacity(children.len());
    let mut selected: Option<usize> = None;
    let mut selectable_index = 0usize;

    for child in &children {
        if child.node_type != "list_item" {
            // Ignore unexpected non-list_item children rather than error —
            // spec treats list children as a list of list_items; mixing
            // primitives is not illegal but has no visual meaning here.
            continue;
        }
        let (row, was_selected) = list_item_row(child, viewport, actions)?;
        if was_selected {
            selected = Some(selectable_index);
        }
        if !row.header {
            selectable_index += 1;
        }
        items.push(row);
    }

    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items,
            selected,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

/// Build a single [`ListItemProps`] from a `list_item` node.
///
/// Returns the row plus `true` iff the source node had `selected = true`.
fn list_item_row(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<(ListItemProps, bool)> {
    // list_item props are a strict subset of tree_item's (both share
    // `selected` + `action`; `expanded` / `notification` are tree-only and
    // simply ignored for a list_item). Reusing `TreeItemPropsV1` keeps the
    // slot-handling code the same across list and tree — if Phase A later
    // adds a dedicated `ListItemPropsV1`, the swap is local.
    let props = decode_props::<TreeItemPropsV1>(&node.props, viewport, "list_item")?;
    let slots = &node.slots;

    let title = slot_first_text(slots, "title", viewport)
        .ok_or_else(|| anyhow!("ui_contract_adapter: list_item missing required `title` slot"))?;
    let subtitle = slot_first_text(slots, "subtitle", viewport);
    let start = slot_first_text(slots, "start", viewport);
    let end = slot_first_text(slots, "end", viewport);

    // Merge `start`, `title`, `end` into the primary content line so all
    // three semantic regions render on a single row. `subtitle` and
    // `detail` become the secondary/tertiary lines used by ratatui's list
    // renderer today.
    let mut primary_spans: Vec<StyledSpan> = Vec::with_capacity(5);
    if let Some(lead) = start {
        primary_spans.push(StyledSpan {
            text: format!("{lead} "),
            style: SpanStyle::default(),
        });
    }
    primary_spans.push(StyledSpan {
        text: title,
        style: SpanStyle {
            bold: props.notification.unwrap_or(false),
            ..SpanStyle::default()
        },
    });
    if let Some(trail) = end {
        primary_spans.push(StyledSpan {
            text: format!("  {trail}"),
            style: SpanStyle::default(),
        });
    }
    let content = StyledContent::Styled(primary_spans);

    let secondary = subtitle.map(StyledContent::Plain);
    let tertiary = slot_first_text(slots, "detail", viewport).map(StyledContent::Plain);

    // F1 + F1b: record the full envelope keyed by the node's stable id
    // (fallback: anon walk-counter) so multiple rows sharing an action id
    // each keep their own payload. Disabled actions drop the legacy
    // string so the existing list-dispatch path treats the row as inert.
    let action = props.action;
    let disabled = action.as_ref().is_some_and(|a| a.disabled.unwrap_or(false));
    let action_id = action
        .as_ref()
        .filter(|_| !disabled)
        .map(|a| a.id.clone());
    if let Some(act) = action {
        actions.insert(node.id.as_deref(), act);
    }

    Ok((
        ListItemProps {
            content,
            secondary,
            tertiary,
            header: false,
            style: None,
            action: action_id,
        },
        props.selected.unwrap_or(false),
    ))
}

fn render_standalone_list_item(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    // A bare list_item outside a list is unusual but valid — wrap it in a
    // single-item list so the TUI's existing list widget handles it.
    let (row, selected) = list_item_row(node, viewport, actions)?;
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: vec![row],
            selected: selected.then_some(0),
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

fn render_tree(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let mut rows: Vec<ListItemProps> = Vec::new();
    let mut selected: Option<usize> = None;
    let mut selectable_index = 0usize;

    let top_level = filter_children(&node.children, viewport);
    for child in &top_level {
        flatten_tree_item(
            child,
            viewport,
            actions,
            0,
            &mut rows,
            &mut selected,
            &mut selectable_index,
        )?;
    }

    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

fn render_standalone_tree_item(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let mut rows: Vec<ListItemProps> = Vec::new();
    let mut selected: Option<usize> = None;
    let mut selectable_index = 0usize;
    flatten_tree_item(
        node,
        viewport,
        actions,
        0,
        &mut rows,
        &mut selected,
        &mut selectable_index,
    )?;
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

/// Flatten a tree_item (plus children) into a stream of list rows with
/// indentation markers per depth.
///
/// The existing TUI list widget has no tree affordance; depth is encoded
/// by leading ASCII box-drawing glyphs so the hierarchy remains visible
/// when rendered. This keeps the adapter additive — no new widget type
/// was introduced for tree rendering.
fn flatten_tree_item(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    depth: usize,
    rows: &mut Vec<ListItemProps>,
    selected: &mut Option<usize>,
    selectable_index: &mut usize,
) -> Result<()> {
    if node.node_type != "tree_item" {
        // Render non-tree_item children unchanged via a fake list row so
        // mis-composed trees do not silently drop nodes.
        if let Some(title) = node.props.get("text").and_then(JsonValue::as_str) {
            rows.push(ListItemProps {
                content: StyledContent::Plain(title.to_string()),
                secondary: None,
                tertiary: None,
                header: false,
                style: None,
                action: None,
            });
            *selectable_index += 1;
        }
        return Ok(());
    }
    let props = decode_props::<TreeItemPropsV1>(&node.props, viewport, "tree_item")?;
    let slots = &node.slots;

    let title = slot_first_text(slots, "title", viewport).ok_or_else(|| {
        anyhow!("ui_contract_adapter: tree_item missing required `title` slot")
    })?;
    let subtitle = slot_first_text(slots, "subtitle", viewport);
    let start = slot_first_text(slots, "start", viewport);
    let end = slot_first_text(slots, "end", viewport);

    let indent = indent_prefix(depth);
    let expansion_glyph = if slots_have_children(slots) {
        if props.expanded.unwrap_or(false) {
            "▾ "
        } else {
            "▸ "
        }
    } else {
        "  "
    };
    let notification_glyph = if props.notification.unwrap_or(false) {
        "● "
    } else {
        ""
    };

    let mut spans: Vec<StyledSpan> = Vec::with_capacity(6);
    spans.push(StyledSpan {
        text: indent,
        style: SpanStyle::default(),
    });
    spans.push(StyledSpan {
        text: expansion_glyph.to_owned(),
        style: SpanStyle {
            fg: Some(crate::tui::render_tree::SpanColor::Gray),
            ..SpanStyle::default()
        },
    });
    if !notification_glyph.is_empty() {
        spans.push(StyledSpan {
            text: notification_glyph.to_owned(),
            style: SpanStyle {
                fg: Some(crate::tui::render_tree::SpanColor::Yellow),
                bold: true,
                ..SpanStyle::default()
            },
        });
    }
    if let Some(lead) = start {
        spans.push(StyledSpan {
            text: format!("{lead} "),
            style: SpanStyle::default(),
        });
    }
    spans.push(StyledSpan {
        text: title,
        style: SpanStyle {
            bold: props.selected.unwrap_or(false),
            ..SpanStyle::default()
        },
    });
    if let Some(trail) = end {
        spans.push(StyledSpan {
            text: format!("  {trail}"),
            style: SpanStyle::default(),
        });
    }

    // F1 + F1b: record the full envelope under a unique per-row key and
    // drop the legacy action string when the envelope is disabled, so the
    // existing list-dispatch path treats the row as inert while the full
    // envelope remains available in the ActionTable.
    let action = props.action.clone();
    let disabled = action.as_ref().is_some_and(|a| a.disabled.unwrap_or(false));
    let action_id = action
        .as_ref()
        .filter(|_| !disabled)
        .map(|a| a.id.clone());
    if let Some(act) = action {
        actions.insert(node.id.as_deref(), act);
    }

    if props.selected.unwrap_or(false) {
        *selected = Some(*selectable_index);
    }

    rows.push(ListItemProps {
        content: StyledContent::Styled(spans),
        secondary: subtitle.map(StyledContent::Plain),
        tertiary: None,
        header: false,
        style: None,
        action: action_id,
    });
    *selectable_index += 1;

    // Recurse into `children` slot only when expanded. This matches the
    // spec's tree behavior — collapsed subtrees are elided.
    if props.expanded.unwrap_or(true) {
        if let Some(children) = slots.get("children") {
            let resolved = filter_children(children, viewport);
            for child in &resolved {
                flatten_tree_item(
                    child,
                    viewport,
                    actions,
                    depth + 1,
                    rows,
                    selected,
                    selectable_index,
                )?;
            }
        }
    }

    Ok(())
}

// =============================================================================
// Internal / experimental primitives
// =============================================================================

fn render_dialog(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<DialogPropsV1>(&node.props, viewport, "dialog")?;

    let mut rows: Vec<RenderNode> = Vec::with_capacity(3);
    let mut constraints: Vec<Constraint> = Vec::with_capacity(3);

    // Title row
    rows.push(paragraph_widget_line(
        &props.title,
        SpanStyle {
            bold: true,
            ..SpanStyle::default()
        },
        ParagraphAlignment::Center,
    ));
    constraints.push(Constraint::Length(1));

    if let Some(body) = node.slots.get("body") {
        let resolved = filter_children(body, viewport);
        let body_nodes = render_children(&resolved, viewport, actions, stores)?;
        if body_nodes.is_empty() {
            // no-op
        } else if body_nodes.len() == 1 {
            rows.push(body_nodes.into_iter().next().expect("len == 1"));
            constraints.push(Constraint::Min(1));
        } else {
            rows.push(RenderNode::VSplit {
                constraints: default_min_zero_constraints(body_nodes.len()),
                children: body_nodes,
            });
            constraints.push(Constraint::Min(1));
        }
    }

    if let Some(footer) = node.slots.get("footer") {
        let resolved = filter_children(footer, viewport);
        let footer_nodes = render_children(&resolved, viewport, actions, stores)?;
        if !footer_nodes.is_empty() {
            rows.push(RenderNode::HSplit {
                constraints: default_min_zero_constraints(footer_nodes.len()),
                children: footer_nodes,
            });
            constraints.push(Constraint::Length(1));
        }
    }

    let inner = RenderNode::VSplit {
        constraints,
        children: rows,
    };

    // When `open = false`, render an Empty placeholder so downstream
    // interpreters (which expect a non-empty tree) still work; the
    // renderer is free to short-circuit in that case.
    if !props.open {
        return Ok(RenderNode::Widget {
            widget_type: WidgetType::Empty,
            id: node.id.clone(),
            block: None,
            custom_lines: None,
            props: None,
        });
    }

    Ok(RenderNode::Centered {
        width_pct: OVERLAY_WIDTH_PCT,
        height_pct: OVERLAY_HEIGHT_PCT,
        child: Box::new(inner),
    })
}

fn render_menu(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    _stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    // Treat menu items as list items under a centred list overlay. This
    // matches the cross-client "TUI: overlay/menu panel bound to selection
    // or focus" behavior described by the adaptive spec.
    let items_slot = node.slots.get("items");
    let items: Vec<UiNodeV1> = items_slot
        .map(|children| filter_children(children, viewport))
        .unwrap_or_default();

    let mut rendered_items: Vec<ListItemProps> = Vec::with_capacity(items.len());
    let mut selected: Option<usize> = None;
    let mut selectable_index = 0usize;
    for item in &items {
        if item.node_type == "menu_item" || item.node_type == "list_item" {
            let (row, was_selected) = list_item_row(item, viewport, actions)?;
            if was_selected {
                selected = Some(selectable_index);
            }
            if !row.header {
                selectable_index += 1;
            }
            rendered_items.push(row);
        }
    }

    let list_node = RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rendered_items,
            selected,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    };

    Ok(RenderNode::Centered {
        width_pct: OVERLAY_WIDTH_PCT,
        height_pct: OVERLAY_HEIGHT_PCT,
        child: Box::new(list_node),
    })
}

fn render_standalone_menu_item(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    _stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    render_standalone_list_item(node, viewport, actions)
}

// =============================================================================
// Helpers
// =============================================================================

/// Decode a node's props (after resolving any responsive sentinels) into
/// the given Phase A props struct.
fn decode_props<T: serde::de::DeserializeOwned>(
    props: &JsonMap<String, JsonValue>,
    viewport: &UiViewportV1,
    ctx: &'static str,
) -> Result<T> {
    let resolved = resolve_props(props, viewport);
    serde_json::from_value(JsonValue::Object(resolved))
        .map_err(|err| anyhow!("ui_contract_adapter: failed to decode `{ctx}` props: {err}"))
}

/// Resolve `Stack.direction` — which may be a scalar or a responsive
/// value — to a concrete [`UiStackDirection`].
fn resolve_stack_direction(props: &StackPropsV1) -> UiStackDirection {
    match props.direction {
        crate::ui_contract::node::UiValueV1::Scalar(direction) => direction,
        // A responsive value that reached here means the sentinel was not
        // resolved upstream. This should not happen because [`decode_props`]
        // runs `resolve_props` first — but if it does, default to vertical
        // (the safer terminal default) and let future telemetry surface
        // the mistake. We intentionally do NOT panic.
        crate::ui_contract::node::UiValueV1::Responsive(_) => UiStackDirection::Vertical,
    }
}

fn render_children(
    children: &[UiNodeV1],
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<Vec<RenderNode>> {
    let mut out = Vec::with_capacity(children.len());
    for child in children {
        out.push(render_ui_node_with_stores(child, viewport, actions, stores)?);
    }
    Ok(out)
}

fn default_min_zero_constraints(count: usize) -> Vec<Constraint> {
    vec![Constraint::Min(0); count]
}

fn build_panel_block(props: &PanelPropsV1) -> Option<BlockConfig> {
    let has_border = matches!(props.border, Some(true));
    let has_title = props.title.is_some();
    if !has_border && !has_title {
        return None;
    }
    Some(BlockConfig {
        title: props.title.as_ref().map(|t| StyledContent::Plain(t.clone())),
        borders: if has_border {
            BorderStyle::All
        } else {
            BorderStyle::None
        },
        border_style: None,
        border_type: None,
    })
}

/// If the node is a [`RenderNode::Widget`], attach the block to it;
/// otherwise wrap the node so borders still render around the innermost
/// widget. Split nodes do not accept a block today, so we simply return
/// them unchanged and rely on the multi-child panel path above to draw
/// a header separately.
fn attach_block_if_widget(
    node: RenderNode,
    block: Option<BlockConfig>,
    id: Option<String>,
) -> RenderNode {
    match node {
        RenderNode::Widget {
            widget_type,
            id: existing_id,
            block: existing_block,
            custom_lines,
            props,
        } => RenderNode::Widget {
            widget_type,
            id: id.or(existing_id),
            block: existing_block.or(block),
            custom_lines,
            props,
        },
        other => other,
    }
}

fn apply_id(node: RenderNode, id: Option<String>) -> RenderNode {
    match node {
        RenderNode::Widget {
            widget_type,
            id: existing,
            block,
            custom_lines,
            props,
        } => RenderNode::Widget {
            widget_type,
            id: id.or(existing),
            block,
            custom_lines,
            props,
        },
        other => other,
    }
}

fn panel_header_widget(props: &PanelPropsV1, id: Option<&str>) -> RenderNode {
    let text = props.title.clone().unwrap_or_default();
    let style = SpanStyle {
        bold: true,
        ..SpanStyle::default()
    };
    let content = single_span(text, style);
    RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: id.map(std::string::ToString::to_string),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment: ParagraphAlignment::Left,
            wrap: false,
        })),
    }
}

fn paragraph_widget_line(
    text: &str,
    style: SpanStyle,
    alignment: ParagraphAlignment,
) -> RenderNode {
    let content = single_span(text, style);
    RenderNode::Widget {
        widget_type: WidgetType::Paragraph,
        id: None,
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::Paragraph(ParagraphProps {
            lines: vec![content],
            alignment,
            wrap: true,
        })),
    }
}

fn button_widget(
    label: &str,
    action: &UiActionV1,
    icon_prefix: Option<&str>,
    tone: Option<crate::tui::render_tree::SpanColor>,
) -> RenderNode {
    let disabled = action.disabled.unwrap_or(false);
    let mut spans: Vec<StyledSpan> = Vec::with_capacity(3);
    if let Some(icon) = icon_prefix {
        spans.push(StyledSpan {
            text: format!("{} ", icon_glyph_for(icon)),
            style: SpanStyle::default(),
        });
    }
    spans.push(StyledSpan {
        text: label.to_owned(),
        style: SpanStyle {
            fg: tone,
            bold: !disabled,
            dim: disabled,
            ..SpanStyle::default()
        },
    });

    let content = StyledContent::Styled(spans);
    let list_item = ListItemProps {
        content,
        secondary: None,
        tertiary: None,
        header: false,
        style: None,
        // Buttons always route through the existing string-action pipeline.
        // The full envelope (payload, disabled) is preserved separately on
        // the ActionTable — see `action.rs` for the rationale.
        action: if disabled {
            None
        } else {
            Some(action.id.clone())
        },
    };
    RenderNode::Widget {
        widget_type: WidgetType::List,
        id: None,
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: vec![list_item],
            selected: Some(0),
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    }
}

/// Render an icon name as a short-ish glyph.
///
/// Terminals have no icon font, so the adapter falls back to the icon id
/// itself wrapped in a single colon marker (`:{name}:`). This keeps the
/// visual present so layouts do not collapse; specific icons can be
/// special-cased later without touching this module's callers.
fn icon_glyph_for(name: &str) -> String {
    match name {
        // Lightly curated set of common Botster icons to short glyphs.
        // Fall-through is `:name:` so unknown icons remain legible.
        "workspace" => "🗂".to_string(),
        "session" => "⎇".to_string(),
        "close" => "✕".to_string(),
        "check" => "✓".to_string(),
        "more" => "…".to_string(),
        "sparkle" => "✦".to_string(),
        other => format!(":{other}:"),
    }
}

/// Generate the indentation prefix for a tree row at the given depth.
fn indent_prefix(depth: usize) -> String {
    // Two visual columns of indentation per depth — enough for keyboard
    // users to see the hierarchy without consuming too much width. The
    // rightmost column adopts a corner glyph to mark the last slot.
    if depth == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(depth * 2);
    for _ in 0..depth.saturating_sub(1) {
        out.push_str("  ");
    }
    out.push_str("└ ");
    out
}

/// Resolve a slot's children through [`filter_children`] so `ui.when` /
/// `ui.hidden` wrappers inside a slot drop correctly instead of being
/// silently skipped (codex F3).
fn resolved_slot(
    slots: &std::collections::BTreeMap<String, Vec<UiChildV1>>,
    key: &str,
    viewport: &UiViewportV1,
) -> Vec<UiNodeV1> {
    slots
        .get(key)
        .map(|children| filter_children(children, viewport))
        .unwrap_or_default()
}

/// Return the first renderable text string found in a slot.
///
/// Walks the slot's children with conditional wrappers already resolved
/// against `viewport`, so `ui.when` / `ui.hidden` elisions are honored.
fn slot_first_text(
    slots: &std::collections::BTreeMap<String, Vec<UiChildV1>>,
    key: &str,
    viewport: &UiViewportV1,
) -> Option<String> {
    for node in resolved_slot(slots, key, viewport) {
        if let Some(text) = text_node_string(&node) {
            return Some(text);
        }
    }
    None
}

/// Extract the displayed string from a content-bearing node.
///
/// Supports `text`, `icon` (label or id), `badge`, `status_dot`
/// (label or the filled-bullet glyph), and anything else with a top-level
/// `text` prop. Compound slots typically host one of these so this covers
/// the common case; richer slot compositions are rendered generically by
/// the walking code elsewhere.
fn text_node_string(node: &UiNodeV1) -> Option<String> {
    match node.node_type.as_str() {
        "text" | "badge" => node
            .props
            .get("text")
            .and_then(JsonValue::as_str)
            .map(std::string::ToString::to_string),
        "icon" => node
            .props
            .get("label")
            .and_then(JsonValue::as_str)
            .map(std::string::ToString::to_string)
            .or_else(|| {
                node.props
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .map(std::string::ToString::to_string)
            }),
        "status_dot" => node
            .props
            .get("label")
            .and_then(JsonValue::as_str)
            .map(std::string::ToString::to_string)
            .or_else(|| Some("\u{25CF}".to_owned())),
        _ => node
            .props
            .get("text")
            .and_then(JsonValue::as_str)
            .map(std::string::ToString::to_string),
    }
}

fn slots_have_children(
    slots: &std::collections::BTreeMap<String, Vec<UiChildV1>>,
) -> bool {
    slots
        .get("children")
        .is_some_and(|children| !children.is_empty())
}

// =============================================================================
// Wire protocol v2 — composite primitives.
//
// These renderers consume zero authored children/slots; they read their data
// from `stores` (the per-entity-type TUI store aggregate) and expand into
// the same flat tree the v1 hub-rendered layout used to ship.
//
// When `stores` is None (legacy entry point), each composite renders a
// minimal placeholder paragraph. The cold-turkey switch in commit 7 wires
// LayoutLua to use `render_lua_ui_node_with_stores` so the placeholder
// path never fires in production.
// =============================================================================

/// Helper used by every v2 composite to short-circuit when the legacy
/// entry point (no stores) was used. Returns a one-line paragraph that
/// makes the missing-stores case visible without spamming the log.
fn placeholder_widget(label: &str) -> RenderNode {
    paragraph_widget_line(label, SpanStyle::default(), ParagraphAlignment::Left)
}

/// Build an action with an inline payload object. `UiActionV1::payload` is a
/// `JsonMap`, so callers that want to attach `{ "key": "value" }` need to
/// thread through `from_value` to convert. This helper hides that wart.
fn action_with_payload(id: &str, payload: JsonValue) -> UiActionV1 {
    let mut act = UiActionV1::new(id);
    if let JsonValue::Object(map) = payload {
        act.payload = map;
    }
    act
}

fn render_session_list(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<SessionListPropsV1>(&node.props, viewport, "session_list")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(session_list)"));
    };
    let Some(session_store) = stores.store("session") else {
        return Ok(placeholder_widget("No sessions yet"));
    };
    if session_store.order.is_empty() {
        return Ok(placeholder_widget("No sessions yet"));
    }

    let density = match &props.density {
        Some(crate::ui_contract::node::UiValueV1::Scalar(d)) => *d,
        _ => UiSurfaceDensity::Panel,
    };
    let grouping = props.grouping.unwrap_or(UiSessionListGrouping::Workspace);

    let workspace_store = stores.store("workspace");
    let mut rows: Vec<ListItemProps> = Vec::with_capacity(session_store.order.len() * 2);
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    let push_session_row = |session: &JsonValue,
                            rows: &mut Vec<ListItemProps>,
                            actions: &mut ActionTable,
                            indent_depth: usize| {
        let title = session
            .get("title")
            .and_then(JsonValue::as_str)
            .or_else(|| session.get("display_name").and_then(JsonValue::as_str))
            .or_else(|| session.get("session_uuid").and_then(JsonValue::as_str))
            .unwrap_or("(unnamed)");
        let session_uuid = session
            .get("session_uuid")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let secondary = session
            .get("subtext")
            .and_then(JsonValue::as_str)
            .map(str::to_owned);
        let action_id = (!session_uuid.is_empty()).then(|| {
            let act = action_with_payload(
                "botster.session.select",
                serde_json::json!({ "sessionUuid": session_uuid }),
            );
            let id = act.id.clone();
            actions.insert(Some(session_uuid), act);
            id
        });
        let indent = indent_prefix(indent_depth);
        let prefix = if matches!(density, UiSurfaceDensity::Sidebar) {
            indent
        } else {
            format!("{indent}  ")
        };
        rows.push(ListItemProps {
            content: StyledContent::Plain(format!("{prefix}{title}")),
            secondary: secondary.map(StyledContent::Plain),
            tertiary: None,
            header: false,
            style: None,
            action: action_id,
        });
    };

    if matches!(grouping, UiSessionListGrouping::Workspace) {
        if let Some(ws_store) = workspace_store {
            for (ws_id, workspace) in ws_store.iter() {
                let header_text = workspace
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or(ws_id);
                rows.push(ListItemProps {
                    content: StyledContent::Plain(header_text.to_owned()),
                    secondary: None,
                    tertiary: None,
                    header: true,
                    style: None,
                    action: None,
                });
                for (sess_id, session) in session_store.iter() {
                    let sess_workspace =
                        session.get("workspace_id").and_then(JsonValue::as_str);
                    if sess_workspace == Some(ws_id.as_str()) {
                        push_session_row(session, &mut rows, actions, 1);
                        seen.insert(sess_id.as_str());
                    }
                }
            }
        }
    }

    // Ungrouped bucket — every session not already attached to a workspace
    // group, or every session if grouping = flat.
    for (sess_id, session) in session_store.iter() {
        if !seen.contains(sess_id.as_str()) {
            push_session_row(session, &mut rows, actions, 0);
        }
    }

    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected: None,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

fn render_workspace_list(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    _actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let _ = decode_props::<WorkspaceListPropsV1>(&node.props, viewport, "workspace_list")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(workspace_list)"));
    };
    let Some(store) = stores.store("workspace") else {
        return Ok(placeholder_widget("No workspaces"));
    };
    if store.order.is_empty() {
        return Ok(placeholder_widget("No workspaces"));
    }
    let rows: Vec<ListItemProps> = store
        .iter()
        .map(|(id, ws)| {
            let name = ws.get("name").and_then(JsonValue::as_str).unwrap_or(id);
            ListItemProps {
                content: StyledContent::Plain(name.to_owned()),
                secondary: None,
                tertiary: None,
                header: false,
                style: None,
                action: None,
            }
        })
        .collect();
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected: None,
            highlight_style: None,
            highlight_symbol: None,
        })),
    })
}

fn render_spawn_target_list(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<SpawnTargetListPropsV1>(&node.props, viewport, "spawn_target_list")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(spawn_target_list)"));
    };
    let Some(store) = stores.store("spawn_target") else {
        return Ok(placeholder_widget("No spawn targets"));
    };
    let select_action_id = props
        .on_select
        .as_ref()
        .map_or("botster.spawn_target.select", |a| a.id.as_str())
        .to_owned();
    let rows: Vec<ListItemProps> = store
        .iter()
        .map(|(id, target)| {
            let label = target
                .get("target_name")
                .and_then(JsonValue::as_str)
                .or_else(|| target.get("target_repo").and_then(JsonValue::as_str))
                .unwrap_or(id);
            // Per-row action: merge the template with target_id into payload.
            let act = action_with_payload(
                &select_action_id,
                serde_json::json!({ "targetId": id }),
            );
            let action_id = act.id.clone();
            actions.insert(Some(id.as_str()), act);
            ListItemProps {
                content: StyledContent::Plain(label.to_owned()),
                secondary: target
                    .get("target_repo")
                    .and_then(JsonValue::as_str)
                    .map(|s| StyledContent::Plain(s.to_owned())),
                tertiary: None,
                header: false,
                style: None,
                action: Some(action_id),
            }
        })
        .collect();
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected: None,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

fn render_worktree_list(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    _actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<WorktreeListPropsV1>(&node.props, viewport, "worktree_list")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(worktree_list)"));
    };
    let Some(store) = stores.store("worktree") else {
        return Ok(placeholder_widget("No worktrees"));
    };
    let rows: Vec<ListItemProps> = store
        .iter()
        .filter(|(_, wt)| {
            wt.get("target_id")
                .and_then(JsonValue::as_str)
                .map_or(false, |t| t == props.target_id)
        })
        .map(|(_id, wt)| {
            let path = wt
                .get("worktree_path")
                .and_then(JsonValue::as_str)
                .or_else(|| wt.get("path").and_then(JsonValue::as_str))
                .unwrap_or("?");
            let branch = wt.get("branch").and_then(JsonValue::as_str);
            ListItemProps {
                content: StyledContent::Plain(path.to_owned()),
                secondary: branch.map(|b| StyledContent::Plain(b.to_owned())),
                tertiary: None,
                header: false,
                style: None,
                action: None,
            }
        })
        .collect();
    if rows.is_empty() {
        return Ok(placeholder_widget("No worktrees"));
    }
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: rows,
            selected: None,
            highlight_style: None,
            highlight_symbol: None,
        })),
    })
}

fn render_session_row(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    _actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let props = decode_props::<SessionRowPropsV1>(&node.props, viewport, "session_row")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(session_row)"));
    };
    let Some(store) = stores.store("session") else {
        return Ok(placeholder_widget("(session_row missing)"));
    };
    let title = store
        .field(&props.session_uuid, "title")
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            store
                .field(&props.session_uuid, "display_name")
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| props.session_uuid.clone());
    Ok(paragraph_widget_line(
        &title,
        SpanStyle::default(),
        ParagraphAlignment::Left,
    ))
}

fn render_hub_recovery_state(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    _actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let _ = decode_props::<HubRecoveryStatePropsV1>(&node.props, viewport, "hub_recovery_state")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(hub_recovery_state)"));
    };
    let Some(store) = stores.store("hub") else {
        return Ok(placeholder_widget("hub: starting"));
    };
    // Singleton: the first (and only) entity carries the recovery payload.
    let state = store
        .iter()
        .next()
        .and_then(|(_, hub)| hub.get("state").cloned())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "starting".to_string());
    Ok(paragraph_widget_line(
        &format!("hub: {state}"),
        SpanStyle::default(),
        ParagraphAlignment::Left,
    ))
}

fn render_connection_code(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    _actions: &mut ActionTable,
    stores: Option<&TuiEntityStores>,
) -> Result<RenderNode> {
    let _ = decode_props::<ConnectionCodePropsV1>(&node.props, viewport, "connection_code")?;
    let Some(stores) = stores else {
        return Ok(placeholder_widget("(connection_code)"));
    };
    let Some(store) = stores.store("connection_code") else {
        return Ok(placeholder_widget("Connection code unavailable"));
    };
    let url = store
        .iter()
        .next()
        .and_then(|(_, cc)| cc.get("url").cloned())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "(no url)".to_string());
    Ok(paragraph_widget_line(
        &url,
        SpanStyle::default(),
        ParagraphAlignment::Left,
    ))
}

fn render_new_session_button(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    let props = decode_props::<NewSessionButtonPropsV1>(&node.props, viewport, "new_session_button")?;
    let action_id = props.action.id.clone();
    actions.insert(node.id.as_deref(), props.action);
    Ok(RenderNode::Widget {
        widget_type: WidgetType::List,
        id: node.id.clone(),
        block: None,
        custom_lines: None,
        props: Some(WidgetProps::List(ListProps {
            items: vec![ListItemProps {
                content: StyledContent::Plain("+ New session".to_owned()),
                secondary: None,
                tertiary: None,
                header: false,
                style: None,
                action: Some(action_id),
            }],
            selected: None,
            highlight_style: Some(SpanStyle {
                reversed: true,
                ..SpanStyle::default()
            }),
            highlight_symbol: Some(BUTTON_HIGHLIGHT_SYMBOL.to_owned()),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_contract::viewport::{UiHeightClass, UiPointer, UiWidthClass};

    fn regular_viewport() -> UiViewportV1 {
        UiViewportV1::new(
            UiWidthClass::Regular,
            UiHeightClass::Regular,
            UiPointer::None,
        )
    }

    #[test]
    fn is_ui_node_type_recognises_all_primitives() {
        for t in UI_NODE_TYPE_NAMES {
            assert!(is_ui_node_type(t), "missing primitive: {t}");
        }
        assert!(!is_ui_node_type("hsplit"));
        assert!(!is_ui_node_type("vsplit"));
        assert!(!is_ui_node_type("centered"));
    }

    #[test]
    fn every_ui_node_type_name_dispatches_in_render_ui_node() {
        // Build a minimal, shape-valid node for each primitive and assert
        // that render_ui_node doesn't return the "unknown primitive"
        // error. We tolerate prop-validation errors because the goal is
        // to prove the dispatcher covers every name, not to stand up a
        // fully-formed surface for each primitive.
        let viewport = regular_viewport();
        for &type_name in UI_NODE_TYPE_NAMES {
            let node = UiNodeV1::new(type_name);
            let mut actions = ActionTable::new();
            let result = render_ui_node(&node, &viewport, &mut actions);
            if let Err(e) = &result {
                let msg = e.to_string();
                assert!(
                    !msg.contains("unknown primitive"),
                    "render_ui_node missing branch for `{type_name}`: {msg}"
                );
            }
        }
    }

    #[test]
    fn indent_prefix_grows_with_depth() {
        assert_eq!(indent_prefix(0), "");
        assert_eq!(indent_prefix(1), "└ ");
        assert_eq!(indent_prefix(3), "    └ ");
    }

    #[test]
    fn button_widget_records_action_id_on_list_item() {
        let action = UiActionV1::new("botster.session.select");
        let render = button_widget("Select", &action, None, None);
        let RenderNode::Widget { props, .. } = render else {
            panic!("expected widget");
        };
        let Some(WidgetProps::List(lp)) = props else {
            panic!("expected list props");
        };
        assert_eq!(lp.items.len(), 1);
        assert_eq!(
            lp.items[0].action.as_deref(),
            Some("botster.session.select")
        );
    }

    #[test]
    fn render_text_produces_paragraph_widget() {
        let mut node = UiNodeV1::new("text");
        node.props.insert("text".into(), JsonValue::from("Hello"));
        let mut actions = ActionTable::new();
        let rendered = render_ui_node(&node, &regular_viewport(), &mut actions).expect("render");
        match rendered {
            RenderNode::Widget {
                widget_type: WidgetType::Paragraph,
                props: Some(WidgetProps::Paragraph(p)),
                ..
            } => {
                assert_eq!(p.lines.len(), 1);
            }
            _ => panic!("expected paragraph widget"),
        }
    }

    // =========================================================================
    // Wire protocol v2 composite renderer tests
    // =========================================================================

    fn populated_stores() -> TuiEntityStores {
        let mut stores = TuiEntityStores::new();

        let workspaces = stores.store_mut("workspace");
        workspaces.apply_snapshot(
            vec![
                serde_json::json!({ "workspace_id": "ws-1", "name": "Roadmap" }),
                serde_json::json!({ "workspace_id": "ws-2", "name": "Triage" }),
            ],
            "workspace_id",
            1,
        );

        let sessions = stores.store_mut("session");
        sessions.apply_snapshot(
            vec![
                serde_json::json!({
                    "session_uuid": "sess-a",
                    "title": "alpha",
                    "workspace_id": "ws-1",
                    "session_type": "agent"
                }),
                serde_json::json!({
                    "session_uuid": "sess-b",
                    "title": "beta",
                    "workspace_id": "ws-1",
                    "session_type": "accessory"
                }),
                serde_json::json!({
                    "session_uuid": "sess-c",
                    "title": "gamma",
                    "session_type": "agent"
                }),
            ],
            "session_uuid",
            1,
        );

        let targets = stores.store_mut("spawn_target");
        targets.apply_snapshot(
            vec![serde_json::json!({
                "target_id": "tgt-1",
                "target_name": "trybotster",
                "target_repo": "Tonksthebear/trybotster"
            })],
            "target_id",
            1,
        );

        let worktrees = stores.store_mut("worktree");
        worktrees.apply_snapshot(
            vec![serde_json::json!({
                "worktree_path": "/tmp/wt-1",
                "target_id": "tgt-1",
                "branch": "feat/x"
            })],
            "worktree_path",
            1,
        );

        let hub = stores.store_mut("hub");
        hub.apply_snapshot(
            vec![serde_json::json!({ "hub_id": "hub-abc", "state": "ready" })],
            "hub_id",
            1,
        );

        let connection_code = stores.store_mut("connection_code");
        connection_code.apply_snapshot(
            vec![serde_json::json!({
                "hub_id": "hub-abc",
                "url": "https://example.test/connect/abc"
            })],
            "hub_id",
            1,
        );

        stores
    }

    fn collect_list_rows(node: &RenderNode) -> Option<&Vec<ListItemProps>> {
        let RenderNode::Widget {
            props: Some(WidgetProps::List(lp)),
            ..
        } = node
        else {
            return None;
        };
        Some(&lp.items)
    }

    #[test]
    fn session_list_with_no_stores_renders_placeholder() {
        let node = UiNodeV1::new("session_list");
        let mut actions = ActionTable::new();
        let rendered = render_ui_node_with_stores(&node, &regular_viewport(), &mut actions, None)
            .expect("render");
        // Placeholder is a single-line paragraph; assertion: it is a
        // paragraph widget, not a list.
        assert!(
            matches!(
                rendered,
                RenderNode::Widget {
                    widget_type: WidgetType::Paragraph,
                    ..
                }
            ),
            "expected placeholder paragraph, got {rendered:?}"
        );
    }

    #[test]
    fn session_list_with_empty_stores_renders_placeholder() {
        let node = UiNodeV1::new("session_list");
        let mut actions = ActionTable::new();
        let stores = TuiEntityStores::new();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        assert!(matches!(
            rendered,
            RenderNode::Widget {
                widget_type: WidgetType::Paragraph,
                ..
            }
        ));
    }

    #[test]
    fn session_list_groups_sessions_under_workspace_headers() {
        let node = UiNodeV1::new("session_list");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let rows = collect_list_rows(&rendered).expect("list rows");
        // Expected ordering:
        //   header(Roadmap) sess-a sess-b header(Triage) sess-c (ungrouped)
        let labels: Vec<String> = rows
            .iter()
            .map(|r| match &r.content {
                StyledContent::Plain(s) => s.clone(),
                StyledContent::Styled(spans) => {
                    spans.iter().map(|s| s.text.as_str()).collect::<String>()
                }
            })
            .collect();
        // Workspace headers appear with header=true.
        assert!(rows.iter().any(|r| r.header), "expected at least one header row");
        // The two ws-1 sessions appear after the Roadmap header.
        let roadmap_idx = labels
            .iter()
            .position(|l| l.contains("Roadmap"))
            .expect("Roadmap header");
        let alpha_idx = labels
            .iter()
            .position(|l| l.contains("alpha"))
            .expect("alpha row");
        let beta_idx = labels
            .iter()
            .position(|l| l.contains("beta"))
            .expect("beta row");
        assert!(roadmap_idx < alpha_idx);
        assert!(roadmap_idx < beta_idx);
        // Ungrouped session "gamma" appears too.
        assert!(labels.iter().any(|l| l.contains("gamma")));
    }

    #[test]
    fn session_list_records_select_action_per_session() {
        let node = UiNodeV1::new("session_list");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        render_ui_node_with_stores(&node, &regular_viewport(), &mut actions, Some(&stores))
            .expect("render");
        // Action table should carry one entry per session keyed by uuid.
        let entry = actions.get("id:sess-a").expect("sess-a action");
        assert_eq!(entry.action.id, "botster.session.select");
        assert_eq!(
            entry.action.payload.get("sessionUuid").and_then(|v| v.as_str()),
            Some("sess-a")
        );
    }

    #[test]
    fn workspace_list_renders_one_row_per_workspace() {
        let node = UiNodeV1::new("workspace_list");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let rows = collect_list_rows(&rendered).expect("list rows");
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn spawn_target_list_attaches_target_id_to_action_payload() {
        let node = UiNodeV1::new("spawn_target_list");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        render_ui_node_with_stores(&node, &regular_viewport(), &mut actions, Some(&stores))
            .expect("render");
        let entry = actions.get("id:tgt-1").expect("tgt-1 action");
        assert_eq!(
            entry.action.payload.get("targetId").and_then(|v| v.as_str()),
            Some("tgt-1")
        );
    }

    #[test]
    fn worktree_list_filters_by_target_id() {
        let mut node = UiNodeV1::new("worktree_list");
        node.props
            .insert("targetId".into(), JsonValue::from("tgt-1"));
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let rows = collect_list_rows(&rendered).expect("list rows");
        assert_eq!(rows.len(), 1);

        // A different target_id returns the empty placeholder.
        let mut other = UiNodeV1::new("worktree_list");
        other
            .props
            .insert("targetId".into(), JsonValue::from("missing"));
        let rendered = render_ui_node_with_stores(
            &other,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        assert!(matches!(
            rendered,
            RenderNode::Widget {
                widget_type: WidgetType::Paragraph,
                ..
            }
        ));
    }

    #[test]
    fn session_row_pulls_title_from_session_store() {
        let mut node = UiNodeV1::new("session_row");
        node.props
            .insert("sessionUuid".into(), JsonValue::from("sess-a"));
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let RenderNode::Widget {
            widget_type: WidgetType::Paragraph,
            props: Some(WidgetProps::Paragraph(p)),
            ..
        } = rendered
        else {
            panic!("expected paragraph widget");
        };
        let line: String = match &p.lines[0] {
            StyledContent::Plain(s) => s.clone(),
            StyledContent::Styled(spans) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
        };
        assert!(line.contains("alpha"), "got {line}");
    }

    #[test]
    fn hub_recovery_state_renders_state_field() {
        let node = UiNodeV1::new("hub_recovery_state");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let RenderNode::Widget {
            props: Some(WidgetProps::Paragraph(p)),
            ..
        } = rendered
        else {
            panic!("expected paragraph");
        };
        let line: String = match &p.lines[0] {
            StyledContent::Plain(s) => s.clone(),
            StyledContent::Styled(spans) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
        };
        assert!(line.contains("ready"), "got {line}");
    }

    #[test]
    fn connection_code_renders_url_field() {
        let node = UiNodeV1::new("connection_code");
        let mut actions = ActionTable::new();
        let stores = populated_stores();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            Some(&stores),
        )
        .expect("render");
        let RenderNode::Widget {
            props: Some(WidgetProps::Paragraph(p)),
            ..
        } = rendered
        else {
            panic!("expected paragraph");
        };
        let line: String = match &p.lines[0] {
            StyledContent::Plain(s) => s.clone(),
            StyledContent::Styled(spans) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
        };
        assert!(line.contains("example.test"), "got {line}");
    }

    #[test]
    fn new_session_button_records_action_in_table() {
        let mut node = UiNodeV1::new("new_session_button");
        let action = serde_json::json!({ "id": "botster.session.create.request" });
        node.props.insert("action".into(), action);
        let mut actions = ActionTable::new();
        let rendered = render_ui_node_with_stores(
            &node,
            &regular_viewport(),
            &mut actions,
            None,
        )
        .expect("render");
        let rows = collect_list_rows(&rendered).expect("list rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].action.as_deref(),
            Some("botster.session.create.request")
        );
    }

    #[test]
    fn render_lua_ui_node_with_stores_resolves_bind_in_text_prop() {
        // End-to-end: a Lua-built tree containing ui.bind() resolves through
        // the new entry point.
        use crate::ui_contract::lua::register as register_ui;
        let lua = mlua::Lua::new();
        register_ui(&lua).expect("register ui");
        let table: mlua::Table = lua
            .load(r#"return ui.text{ text = ui.bind("/session/sess-a/title") }"#)
            .eval()
            .expect("Lua eval");

        let stores = populated_stores();
        let (rendered, _actions) = super::super::render_lua_ui_node_with_stores(
            &lua,
            &table,
            &regular_viewport(),
            Some(&stores),
        )
        .expect("render with stores");
        let RenderNode::Widget {
            props: Some(WidgetProps::Paragraph(p)),
            ..
        } = rendered
        else {
            panic!("expected paragraph");
        };
        let line: String = match &p.lines[0] {
            StyledContent::Plain(s) => s.clone(),
            StyledContent::Styled(spans) => {
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            }
        };
        assert_eq!(line, "alpha");
    }
}
