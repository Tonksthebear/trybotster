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

use crate::tui::render_tree::{
    BlockConfig, BorderStyle, ListItemProps, ListProps, ParagraphAlignment, ParagraphProps,
    RenderNode, SpanStyle, StyledContent, StyledSpan, WidgetProps, WidgetType,
};
use crate::ui_contract::node::{UiActionV1, UiChildV1, UiNodeV1};
use crate::ui_contract::props::{
    BadgePropsV1, ButtonPropsV1, DialogPropsV1, EmptyStatePropsV1, IconButtonPropsV1, IconPropsV1,
    PanelPropsV1, StackPropsV1, StatusDotPropsV1, TextPropsV1, TreeItemPropsV1,
};
use crate::ui_contract::tokens::UiStackDirection;
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
/// # Errors
///
/// Returns an error if the node has an unknown primitive type, or if its
/// `props` do not deserialise into the expected Phase A props struct.
pub fn render_ui_node(
    node: &UiNodeV1,
    viewport: &UiViewportV1,
    actions: &mut ActionTable,
) -> Result<RenderNode> {
    match node.node_type.as_str() {
        "stack" => render_stack(node, viewport, actions),
        "inline" => render_inline(node, viewport, actions),
        "panel" => render_panel(node, viewport, actions),
        "scroll_area" => render_scroll_area(node, viewport, actions),
        "overlay" => render_overlay(node, viewport, actions),
        "text" => render_text(node, viewport),
        "icon" => render_icon(node, viewport),
        "badge" => render_badge(node, viewport),
        "status_dot" => render_status_dot(node, viewport),
        "empty_state" => render_empty_state(node, viewport, actions),
        "button" => render_button(node, viewport, actions),
        "icon_button" => render_icon_button(node, viewport, actions),
        "list" => render_list(node, viewport, actions),
        "list_item" => render_standalone_list_item(node, viewport, actions),
        "tree" => render_tree(node, viewport, actions),
        "tree_item" => render_standalone_tree_item(node, viewport, actions),
        "dialog" => render_dialog(node, viewport, actions),
        "menu" => render_menu(node, viewport, actions),
        "menu_item" => render_standalone_menu_item(node, viewport, actions),
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
) -> Result<RenderNode> {
    let props = decode_props::<StackPropsV1>(&node.props, viewport, "stack")?;
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions)?;
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
) -> Result<RenderNode> {
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions)?;
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
            let inner = render_ui_node(&children[0], viewport, actions)?;
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
                rendered.push(render_ui_node(child, viewport, actions)?);
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
) -> Result<RenderNode> {
    // The TUI has no dedicated scroll widget today — pass the children
    // through in a vertical stack so the layout constraints inherited
    // from the parent still bound the area. Documented under "Known
    // limits" in this module's doc comment.
    let children = filter_children(&node.children, viewport);
    let rendered = render_children(&children, viewport, actions)?;
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
) -> Result<RenderNode> {
    // `overlay` is not Lua-public in Phase A but the mapping table in the
    // cross-client spec names it explicitly: wrap the first non-null
    // child in a centred area.
    let children = filter_children(&node.children, viewport);
    let first = children
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("ui_contract_adapter: overlay requires at least one child"))?;
    let child = render_ui_node(&first, viewport, actions)?;
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
        let body_nodes = render_children(&resolved, viewport, actions)?;
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
        let footer_nodes = render_children(&resolved, viewport, actions)?;
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
) -> Result<Vec<RenderNode>> {
    let mut out = Vec::with_capacity(children.len());
    for child in children {
        out.push(render_ui_node(child, viewport, actions)?);
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
}
