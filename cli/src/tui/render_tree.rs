//! Declarative render tree for Lua-driven TUI layout.
//!
//! Lua defines the layout structure (splits, constraints, widget placement),
//! Rust interprets it into ratatui calls. Widget implementations stay in Rust.
//!
//! # Render Tree
//!
//! ```text
//! RenderNode
//!   ├── HSplit { constraints, children }
//!   ├── VSplit { constraints, children }
//!   ├── Centered { width_pct, height_pct, child }
//!   └── Widget { widget_type, block_config }
//! ```
//!
//! # Flow
//!
//! ```text
//! Lua render(state) → table → RenderNode::from_lua_table() → interpret_tree()
//! ```

use anyhow::{anyhow, Result};
use mlua::{Table as LuaTable, Value as LuaValue};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear},
    Frame,
};

use super::render::RenderContext;
use crate::app::centered_rect;

/// A node in the declarative render tree.
#[derive(Debug, Clone)]
pub enum RenderNode {
    /// Horizontal split (children arranged left to right).
    HSplit {
        /// Layout constraints for each child.
        constraints: Vec<Constraint>,
        /// Child nodes.
        children: Vec<RenderNode>,
    },
    /// Vertical split (children arranged top to bottom).
    VSplit {
        /// Layout constraints for each child.
        constraints: Vec<Constraint>,
        /// Child nodes.
        children: Vec<RenderNode>,
    },
    /// Centered overlay (for modals).
    Centered {
        /// Width as percentage of parent.
        width_pct: u16,
        /// Height as percentage of parent.
        height_pct: u16,
        /// Child node rendered inside.
        child: Box<RenderNode>,
    },
    /// Leaf widget node.
    Widget {
        /// Which Rust widget to render.
        widget_type: WidgetType,
        /// Optional block (border/title) wrapping the widget.
        block: Option<BlockConfig>,
        /// Optional custom text lines from Lua, overriding Rust defaults.
        custom_lines: Option<Vec<StyledContent>>,
        /// Optional widget-specific props (e.g., PTY binding for terminal).
        props: Option<WidgetProps>,
    },
}

/// PTY binding for a terminal widget.
///
/// Specifies which agent and PTY session to render. `None` fields
/// default to the currently selected agent/PTY at render time.
#[derive(Debug, Clone, Default)]
pub struct TerminalBinding {
    /// Agent index (defaults to selected agent if `None`).
    pub agent_index: Option<usize>,
    /// PTY index (defaults to active PTY if `None`).
    pub pty_index: Option<usize>,
}

/// Widget-specific props parsed from Lua.
#[derive(Debug, Clone)]
pub enum WidgetProps {
    /// Terminal widget binding to a specific PTY.
    Terminal(TerminalBinding),
    /// List widget props.
    List(ListProps),
    /// Paragraph widget props.
    Paragraph(ParagraphProps),
    /// Input widget props.
    Input(InputProps),
}

/// Generic UI widget types.
///
/// Lua refers to these by string name. Rust renders them with zero
/// application knowledge — all content, styling, and behavior comes
/// from Lua via props.
#[derive(Debug, Clone)]
pub enum WidgetType {
    /// Terminal panel showing vt100 PTY output.
    Terminal,
    /// Generic selectable list with optional headers.
    List,
    /// Static styled text block.
    Paragraph,
    /// Text input with prompt lines.
    Input,
    /// QR code / connection code display (special rendering).
    ConnectionCode,
    /// Empty placeholder — renders just the block border/title.
    Empty,
}

// =============================================================================
// Generic Widget Props
// =============================================================================

/// Props for a generic list widget.
#[derive(Debug, Clone)]
pub struct ListProps {
    /// Items to display.
    pub items: Vec<ListItemProps>,
    /// Index of the selected item among selectable (non-header) items.
    pub selected: Option<usize>,
    /// Style applied to the highlighted item.
    pub highlight_style: Option<SpanStyle>,
    /// Symbol prepended to the highlighted item (e.g., "> ").
    pub highlight_symbol: Option<String>,
}

/// A single item in a generic list.
#[derive(Debug, Clone)]
pub struct ListItemProps {
    /// The display content (plain string or styled spans).
    pub content: StyledContent,
    /// If true, this item is a non-selectable header (rendered dim+bold).
    pub header: bool,
    /// Optional per-item style override.
    pub style: Option<SpanStyle>,
    /// Optional action identifier triggered when this item is selected.
    pub action: Option<String>,
}

/// Props for a paragraph widget.
#[derive(Debug, Clone)]
pub struct ParagraphProps {
    /// Lines of styled content.
    pub lines: Vec<StyledContent>,
    /// Text alignment.
    pub alignment: ParagraphAlignment,
    /// Whether to wrap long lines.
    pub wrap: bool,
}

/// Paragraph text alignment.
#[derive(Debug, Clone, Default)]
pub enum ParagraphAlignment {
    /// Left-aligned text.
    #[default]
    Left,
    /// Center-aligned text.
    Center,
    /// Right-aligned text.
    Right,
}

/// Props for a text input widget.
#[derive(Debug, Clone)]
pub struct InputProps {
    /// Prompt lines displayed above the input.
    pub lines: Vec<StyledContent>,
    /// Current input value.
    pub value: String,
    /// Text alignment.
    pub alignment: ParagraphAlignment,
}

/// Configuration for a ratatui Block (border + title).
#[derive(Debug, Clone)]
pub struct BlockConfig {
    /// Block title (plain string or styled spans).
    pub title: Option<StyledContent>,
    /// Border style.
    pub borders: BorderStyle,
    /// Border styling (color, bold, etc.). Applied via `Block::border_style()`.
    pub border_style: Option<SpanStyle>,
}

/// Border style for blocks.
#[derive(Debug, Clone, Default)]
pub enum BorderStyle {
    /// No borders.
    None,
    /// Borders on all sides.
    #[default]
    All,
}

// =============================================================================
// Styled Content Types
// =============================================================================

/// A line of styled text: either a plain string or a sequence of styled spans.
///
/// Parsed from Lua values where strings produce `Plain` and arrays of
/// `{ text, style }` tables produce `Styled`. Converts to ratatui `Line`
/// via [`to_line`](Self::to_line).
#[derive(Debug, Clone)]
pub enum StyledContent {
    /// Plain unformatted text.
    Plain(String),
    /// Sequence of individually styled spans.
    Styled(Vec<StyledSpan>),
}

/// A styled text span parsed from Lua.
#[derive(Debug, Clone)]
pub struct StyledSpan {
    /// The text content.
    pub text: String,
    /// Styling attributes.
    pub style: SpanStyle,
}

/// Styling attributes for a span.
///
/// Maps to a subset of ratatui `Style`. All fields default to off/none.
#[derive(Debug, Clone, Default)]
pub struct SpanStyle {
    /// Foreground color.
    pub fg: Option<SpanColor>,
    /// Background color.
    pub bg: Option<SpanColor>,
    /// Bold text.
    pub bold: bool,
    /// Dim text.
    pub dim: bool,
    /// Reversed (highlighted) text.
    pub reversed: bool,
    /// Italic text.
    pub italic: bool,
}

/// Named terminal colors.
#[derive(Debug, Clone)]
pub enum SpanColor {
    /// Cyan.
    Cyan,
    /// Green.
    Green,
    /// Red.
    Red,
    /// Yellow.
    Yellow,
    /// White.
    White,
    /// Gray.
    Gray,
    /// Blue.
    Blue,
    /// Magenta.
    Magenta,
}

impl PartialEq<&str> for StyledContent {
    fn eq(&self, other: &&str) -> bool {
        self.as_plain_str() == Some(*other)
    }
}

impl StyledContent {
    /// Returns the inner string if this is a `Plain` variant, or `None`.
    #[must_use]
    pub fn as_plain_str(&self) -> Option<&str> {
        match self {
            Self::Plain(s) => Some(s.as_str()),
            Self::Styled(_) => None,
        }
    }

    /// Convert to a ratatui `Line`.
    #[must_use]
    pub fn to_line(&self) -> Line<'static> {
        match self {
            Self::Plain(s) => Line::from(s.clone()),
            Self::Styled(spans) => {
                let ratatui_spans: Vec<Span<'static>> = spans
                    .iter()
                    .map(|s| Span::styled(s.text.clone(), s.style.to_ratatui_style()))
                    .collect();
                Line::from(ratatui_spans)
            }
        }
    }
}

impl SpanStyle {
    /// Convert to a ratatui `Style`.
    #[must_use]
    pub fn to_ratatui_style(&self) -> Style {
        let mut style = Style::default();

        if let Some(ref fg) = self.fg {
            style = style.fg(fg.to_ratatui_color());
        }
        if let Some(ref bg) = self.bg {
            style = style.bg(bg.to_ratatui_color());
        }

        let mut modifiers = Modifier::empty();
        if self.bold {
            modifiers |= Modifier::BOLD;
        }
        if self.dim {
            modifiers |= Modifier::DIM;
        }
        if self.reversed {
            modifiers |= Modifier::REVERSED;
        }
        if self.italic {
            modifiers |= Modifier::ITALIC;
        }
        if !modifiers.is_empty() {
            style = style.add_modifier(modifiers);
        }

        style
    }
}

impl SpanColor {
    /// Convert to a ratatui `Color`.
    #[must_use]
    pub fn to_ratatui_color(&self) -> Color {
        match self {
            Self::Cyan => Color::Cyan,
            Self::Green => Color::Green,
            Self::Red => Color::Red,
            Self::Yellow => Color::Yellow,
            Self::White => Color::White,
            Self::Gray => Color::Gray,
            Self::Blue => Color::Blue,
            Self::Magenta => Color::Magenta,
        }
    }
}

// =============================================================================
// Constraint Parsing
// =============================================================================

/// Parse a constraint string into a ratatui `Constraint`.
///
/// Supported formats:
/// - `"30%"` → `Constraint::Percentage(30)`
/// - `"20"` → `Constraint::Length(20)`
/// - `"min:10"` → `Constraint::Min(10)`
/// - `"max:80"` → `Constraint::Max(80)`
pub fn parse_constraint(s: &str) -> Result<Constraint> {
    let s = s.trim();

    if let Some(pct) = s.strip_suffix('%') {
        let val: u16 = pct
            .parse()
            .map_err(|_| anyhow!("Invalid percentage constraint: {s}"))?;
        Ok(Constraint::Percentage(val))
    } else if let Some(min) = s.strip_prefix("min:") {
        let val: u16 = min
            .parse()
            .map_err(|_| anyhow!("Invalid min constraint: {s}"))?;
        Ok(Constraint::Min(val))
    } else if let Some(max) = s.strip_prefix("max:") {
        let val: u16 = max
            .parse()
            .map_err(|_| anyhow!("Invalid max constraint: {s}"))?;
        Ok(Constraint::Max(val))
    } else {
        let val: u16 = s
            .parse()
            .map_err(|_| anyhow!("Invalid length constraint: {s}"))?;
        Ok(Constraint::Length(val))
    }
}

// =============================================================================
// Lua Table Deserialization
// =============================================================================

impl RenderNode {
    /// Deserialize a Lua table into a `RenderNode`.
    ///
    /// Expected table format:
    /// ```lua
    /// { type = "hsplit", constraints = { "30%", "70%" }, children = { ... } }
    /// { type = "list", block = { title = "Agents", borders = "all" }, props = { items = {...} } }
    /// { type = "centered", width = 50, height = 40, child = { ... } }
    /// ```
    pub fn from_lua_table(table: &LuaTable) -> Result<Self> {
        let node_type: String = table
            .get("type")
            .map_err(|e| anyhow!("RenderNode missing 'type' field: {e}"))?;

        match node_type.as_str() {
            "hsplit" => Self::parse_split(table, Direction::Horizontal),
            "vsplit" => Self::parse_split(table, Direction::Vertical),
            "centered" => Self::parse_centered(table),
            _ => Self::parse_widget(table, &node_type),
        }
    }

    fn parse_split(table: &LuaTable, direction: Direction) -> Result<Self> {
        // Parse constraints array
        let constraints_table: LuaTable = table
            .get("constraints")
            .map_err(|e| anyhow!("Split node missing 'constraints': {e}"))?;

        let mut constraints = Vec::new();
        for pair in constraints_table.sequence_values::<String>() {
            let s = pair.map_err(|e| anyhow!("Invalid constraint value: {e}"))?;
            constraints.push(parse_constraint(&s)?);
        }

        // Parse children array
        let children_table: LuaTable = table
            .get("children")
            .map_err(|e| anyhow!("Split node missing 'children': {e}"))?;

        let mut children = Vec::new();
        for pair in children_table.sequence_values::<LuaTable>() {
            let child_table = pair.map_err(|e| anyhow!("Invalid child node: {e}"))?;
            children.push(Self::from_lua_table(&child_table)?);
        }

        if constraints.len() != children.len() {
            return Err(anyhow!(
                "Split node has {} constraints but {} children",
                constraints.len(),
                children.len()
            ));
        }

        match direction {
            Direction::Horizontal => Ok(RenderNode::HSplit {
                constraints,
                children,
            }),
            Direction::Vertical => Ok(RenderNode::VSplit {
                constraints,
                children,
            }),
        }
    }

    fn parse_centered(table: &LuaTable) -> Result<Self> {
        let width_pct: u16 = table
            .get("width")
            .map_err(|e| anyhow!("Centered node missing 'width': {e}"))?;
        let height_pct: u16 = table
            .get("height")
            .map_err(|e| anyhow!("Centered node missing 'height': {e}"))?;

        let child_table: LuaTable = table
            .get("child")
            .map_err(|e| anyhow!("Centered node missing 'child': {e}"))?;

        let child = Self::from_lua_table(&child_table)?;

        Ok(RenderNode::Centered {
            width_pct,
            height_pct,
            child: Box::new(child),
        })
    }

    fn parse_widget(table: &LuaTable, type_name: &str) -> Result<Self> {
        let widget_type = match type_name {
            "terminal" => WidgetType::Terminal,
            "list" => WidgetType::List,
            "paragraph" => WidgetType::Paragraph,
            "input" => WidgetType::Input,
            "connection_code" => WidgetType::ConnectionCode,
            "empty" => WidgetType::Empty,
            _ => {
                return Err(anyhow!("Unknown widget type: '{type_name}'"));
            }
        };

        let block = parse_block_config(table);
        let custom_lines = parse_styled_lines(table, "lines").ok();
        let props = match widget_type {
            WidgetType::Terminal => parse_terminal_props(table),
            WidgetType::List => parse_list_props(table),
            WidgetType::Paragraph => parse_paragraph_props(table),
            WidgetType::Input => parse_input_props(table),
            _ => None,
        };

        Ok(RenderNode::Widget {
            widget_type,
            block,
            custom_lines,
            props,
        })
    }
}

/// Parse optional terminal props from a widget table.
///
/// Reads `props.agent_index` and `props.pty_index` if present.
fn parse_terminal_props(table: &LuaTable) -> Option<WidgetProps> {
    let props_value: LuaValue = table.get("props").ok()?;
    let LuaValue::Table(props_table) = props_value else {
        return None;
    };

    let agent_index: Option<usize> = props_table.get("agent_index").ok();
    let pty_index: Option<usize> = props_table.get("pty_index").ok();

    // Only create props if at least one field is specified
    if agent_index.is_some() || pty_index.is_some() {
        Some(WidgetProps::Terminal(TerminalBinding {
            agent_index,
            pty_index,
        }))
    } else {
        None
    }
}

/// Parse list widget props from a Lua table.
///
/// Items can be plain strings or tables with `text`, `header`, and `style` fields.
fn parse_list_props(table: &LuaTable) -> Option<WidgetProps> {
    let props_value: LuaValue = table.get("props").ok()?;
    let LuaValue::Table(props_table) = props_value else { return None; };

    let items_table: LuaTable = props_table.get("items").ok()?;
    let mut items = Vec::new();

    for val in items_table.sequence_values::<LuaValue>() {
        let v = match val {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v {
            LuaValue::String(s) => {
                items.push(ListItemProps {
                    content: StyledContent::Plain(s.to_string_lossy().to_string()),
                    header: false,
                    style: None,
                    action: None,
                });
            }
            LuaValue::Table(item_table) => {
                let text_val: LuaValue = match item_table.get("text") {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let content = match parse_styled_content(&text_val) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let header: bool = item_table.get("header").unwrap_or(false);
                let style = item_table
                    .get::<LuaValue>("style")
                    .ok()
                    .and_then(|v| parse_span_style(&v).ok());
                let action: Option<String> = item_table.get("action").ok();
                items.push(ListItemProps { content, header, style, action });
            }
            _ => continue,
        }
    }

    let selected: Option<usize> = props_table.get("selected").ok();
    let highlight_style = props_table
        .get::<LuaValue>("highlight_style")
        .ok()
        .and_then(|v| parse_span_style(&v).ok());
    let highlight_symbol: Option<String> = props_table.get("highlight_symbol").ok();

    Some(WidgetProps::List(ListProps {
        items,
        selected,
        highlight_style,
        highlight_symbol,
    }))
}

/// Parse paragraph widget props from a Lua table.
fn parse_paragraph_props(table: &LuaTable) -> Option<WidgetProps> {
    let props_value: LuaValue = table.get("props").ok()?;
    let LuaValue::Table(props_table) = props_value else { return None; };

    let lines = parse_styled_lines(&props_table, "lines").unwrap_or_default();

    let alignment_str: Option<String> = props_table.get("alignment").ok();
    let alignment = match alignment_str.as_deref() {
        Some("center") => ParagraphAlignment::Center,
        Some("right") => ParagraphAlignment::Right,
        _ => ParagraphAlignment::Left,
    };

    let wrap: bool = props_table.get("wrap").unwrap_or(false);

    Some(WidgetProps::Paragraph(ParagraphProps {
        lines,
        alignment,
        wrap,
    }))
}

/// Parse input widget props from a Lua table.
fn parse_input_props(table: &LuaTable) -> Option<WidgetProps> {
    let props_value: LuaValue = table.get("props").ok()?;
    let LuaValue::Table(props_table) = props_value else { return None; };

    let lines = parse_styled_lines(&props_table, "lines").unwrap_or_default();
    let value: String = props_table.get("value").unwrap_or_default();

    let alignment_str: Option<String> = props_table.get("alignment").ok();
    let alignment = match alignment_str.as_deref() {
        Some("center") => ParagraphAlignment::Center,
        Some("right") => ParagraphAlignment::Right,
        _ => ParagraphAlignment::Left,
    };

    Some(WidgetProps::Input(InputProps {
        lines,
        value,
        alignment,
    }))
}

/// Parse optional block config from a table.
fn parse_block_config(table: &LuaTable) -> Option<BlockConfig> {
    let block_value: LuaValue = table.get("block").ok()?;

    let LuaValue::Table(block_table) = block_value else {
        return None;
    };

    let title: Option<StyledContent> = block_table
        .get::<LuaValue>("title")
        .ok()
        .and_then(|v| parse_styled_content(&v).ok());

    let borders_str: Option<String> = block_table.get("borders").ok();
    let borders = match borders_str.as_deref() {
        Some("none") => BorderStyle::None,
        _ => BorderStyle::All,
    };

    let border_style = block_table
        .get::<LuaValue>("border_style")
        .ok()
        .and_then(|v| parse_span_style(&v).ok());

    Some(BlockConfig { title, borders, border_style })
}

// =============================================================================
// Styled Content Parsing
// =============================================================================

/// Parse a Lua value into styled content.
///
/// Accepts either:
/// - A plain string → `StyledContent::Plain`
/// - An array of span tables/strings → `StyledContent::Styled`
fn parse_styled_content(value: &LuaValue) -> Result<StyledContent> {
    match value {
        LuaValue::String(s) => {
            Ok(StyledContent::Plain(s.to_string_lossy()))
        }
        LuaValue::Table(table) => {
            let mut spans = Vec::new();
            for val in table.clone().sequence_values::<LuaValue>() {
                let v = val.map_err(|e| anyhow!("Invalid span in styled content: {e}"))?;
                spans.push(parse_styled_span(&v)?);
            }
            Ok(StyledContent::Styled(spans))
        }
        LuaValue::Nil => Err(anyhow!("Styled content is nil")),
        _ => Err(anyhow!("Styled content must be a string or table")),
    }
}

/// Parse a single styled span from a Lua value.
///
/// Accepts either:
/// - A bare string → span with default style
/// - A table with `text` and optional `style` fields
fn parse_styled_span(value: &LuaValue) -> Result<StyledSpan> {
    match value {
        LuaValue::String(s) => {
            Ok(StyledSpan {
                text: s.to_string_lossy(),
                style: SpanStyle::default(),
            })
        }
        LuaValue::Table(table) => {
            let text: String = table
                .get("text")
                .map_err(|e| anyhow!("Span missing 'text' field: {e}"))?;
            let style = match table.get::<LuaValue>("style") {
                Ok(style_val) => parse_span_style(&style_val)?,
                Err(_) => SpanStyle::default(),
            };
            Ok(StyledSpan { text, style })
        }
        _ => Err(anyhow!("Span must be a string or table")),
    }
}

/// Parse a span style from a Lua value.
///
/// Accepts either:
/// - A shorthand string: `"bold"`, `"dim"`, `"reversed"`, `"italic"`
/// - A table: `{ fg = "cyan", bold = true, dim = true }`
fn parse_span_style(value: &LuaValue) -> Result<SpanStyle> {
    match value {
        LuaValue::String(s) => {
            let name = s.to_string_lossy();
            let mut style = SpanStyle::default();
            match name.as_ref() {
                "bold" => style.bold = true,
                "dim" => style.dim = true,
                "reversed" => style.reversed = true,
                "italic" => style.italic = true,
                _ => return Err(anyhow!("Unknown style shorthand: '{name}'")),
            }
            Ok(style)
        }
        LuaValue::Table(table) => {
            let fg = table
                .get::<Option<String>>("fg")
                .unwrap_or(None)
                .map(|s| parse_span_color(&s))
                .transpose()?;
            let bg = table
                .get::<Option<String>>("bg")
                .unwrap_or(None)
                .map(|s| parse_span_color(&s))
                .transpose()?;
            let bold = table.get::<Option<bool>>("bold").unwrap_or(None).unwrap_or(false);
            let dim = table.get::<Option<bool>>("dim").unwrap_or(None).unwrap_or(false);
            let reversed = table.get::<Option<bool>>("reversed").unwrap_or(None).unwrap_or(false);
            let italic = table.get::<Option<bool>>("italic").unwrap_or(None).unwrap_or(false);

            Ok(SpanStyle {
                fg,
                bg,
                bold,
                dim,
                reversed,
                italic,
            })
        }
        LuaValue::Nil => Ok(SpanStyle::default()),
        _ => Err(anyhow!("Style must be a string or table")),
    }
}

/// Parse a named color string.
fn parse_span_color(s: &str) -> Result<SpanColor> {
    match s {
        "cyan" => Ok(SpanColor::Cyan),
        "green" => Ok(SpanColor::Green),
        "red" => Ok(SpanColor::Red),
        "yellow" => Ok(SpanColor::Yellow),
        "white" => Ok(SpanColor::White),
        "gray" | "grey" => Ok(SpanColor::Gray),
        "blue" => Ok(SpanColor::Blue),
        "magenta" => Ok(SpanColor::Magenta),
        _ => Err(anyhow!("Unknown color: '{s}'")),
    }
}

/// Parse an array of styled content lines from a table field.
///
/// Each element can be a plain string or an array of styled spans.
fn parse_styled_lines(table: &LuaTable, key: &str) -> Result<Vec<StyledContent>> {
    let arr: LuaTable = table
        .get(key)
        .map_err(|e| anyhow!("Missing array field '{key}': {e}"))?;

    let mut result = Vec::new();
    for val in arr.clone().sequence_values::<LuaValue>() {
        let v = val.map_err(|e| anyhow!("Invalid value in '{key}': {e}"))?;
        result.push(parse_styled_content(&v)?);
    }
    Ok(result)
}

// =============================================================================
// Block Config → ratatui Block
// =============================================================================

impl BlockConfig {
    /// Convert to a ratatui `Block` widget.
    ///
    /// Styled titles are converted to `Line<'static>` for ratatui compatibility.
    #[must_use]
    pub fn to_block(&self) -> Block<'static> {
        let mut block = Block::default();

        match self.borders {
            BorderStyle::All => {
                block = block.borders(Borders::ALL);
            }
            BorderStyle::None => {}
        }

        if let Some(ref title) = self.title {
            block = block.title(title.to_line());
        }

        if let Some(ref style) = self.border_style {
            block = block.border_style(style.to_ratatui_style());
        }

        block
    }
}

// =============================================================================
// Tree Interpreter
// =============================================================================

/// Interpret a render tree, rendering each node to the given frame area.
///
/// Recursively walks the tree, splitting areas for layout nodes and
/// dispatching to Rust widget implementations for leaf nodes.
pub fn interpret_tree(node: &RenderNode, f: &mut Frame, ctx: &RenderContext, area: Rect) {
    match node {
        RenderNode::HSplit {
            constraints,
            children,
        } => {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(constraints.as_slice())
                .split(area);

            for (child, chunk) in children.iter().zip(chunks.iter()) {
                interpret_tree(child, f, ctx, *chunk);
            }
        }
        RenderNode::VSplit {
            constraints,
            children,
        } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints.as_slice())
                .split(area);

            for (child, chunk) in children.iter().zip(chunks.iter()) {
                interpret_tree(child, f, ctx, *chunk);
            }
        }
        RenderNode::Centered {
            width_pct,
            height_pct,
            child,
        } => {
            let centered_area = centered_rect(*width_pct, *height_pct, area);
            f.render_widget(Clear, centered_area);
            interpret_tree(child, f, ctx, centered_area);
        }
        RenderNode::Widget {
            widget_type,
            block,
            custom_lines,
            props,
        } => {
            render_widget(widget_type, block.as_ref(), custom_lines.as_deref(), props.as_ref(), f, ctx, area);
        }
    }
}

/// Render a leaf widget using existing Rust rendering functions.
fn render_widget(
    widget_type: &WidgetType,
    block_cfg: Option<&BlockConfig>,
    custom_lines: Option<&[StyledContent]>,
    props: Option<&WidgetProps>,
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
) {
    let block = block_cfg.map(BlockConfig::to_block).unwrap_or_default();

    match widget_type {
        WidgetType::Terminal => {
            let binding = props.and_then(|p| match p {
                WidgetProps::Terminal(b) => Some(b),
                _ => None,
            });
            super::render::render_terminal_panel(f, ctx, area, block, binding);
        }
        WidgetType::List => {
            if let Some(WidgetProps::List(list_props)) = props {
                super::render::render_list_widget(f, area, block, list_props);
            } else {
                f.render_widget(block, area);
            }
        }
        WidgetType::Paragraph => {
            if let Some(WidgetProps::Paragraph(para_props)) = props {
                super::render::render_paragraph_widget(f, area, block, para_props);
            } else if let Some(lines) = custom_lines {
                // Fallback: use custom_lines if no props
                let text: Vec<Line> = lines.iter().map(StyledContent::to_line).collect();
                let paragraph = ratatui::widgets::Paragraph::new(text)
                    .block(block)
                    .alignment(ratatui::layout::Alignment::Left)
                    .wrap(ratatui::widgets::Wrap { trim: false });
                f.render_widget(paragraph, area);
            } else {
                f.render_widget(block, area);
            }
        }
        WidgetType::Input => {
            if let Some(WidgetProps::Input(input_props)) = props {
                super::render::render_input_widget(f, area, block, input_props);
            } else {
                f.render_widget(block, area);
            }
        }
        WidgetType::ConnectionCode => {
            super::render::render_connection_code_widget(f, ctx, area, block, custom_lines);
        }
        WidgetType::Empty => {
            f.render_widget(block, area);
        }
    }
}

// =============================================================================
// Tree Binding Collection
// =============================================================================

/// Collect all terminal bindings from a render tree.
///
/// Walks the tree recursively, collecting `(agent_index, pty_index)` pairs
/// from every `Terminal` widget. Unspecified fields in bindings are resolved
/// using the provided defaults (typically the selected agent/active PTY).
///
/// Used by `sync_subscriptions` to determine which PTYs the layout needs.
pub fn collect_terminal_bindings(
    node: &RenderNode,
    default_agent: usize,
    default_pty: usize,
) -> std::collections::HashSet<(usize, usize)> {
    let mut set = std::collections::HashSet::new();
    collect_bindings_recursive(node, default_agent, default_pty, &mut set);
    set
}

fn collect_bindings_recursive(
    node: &RenderNode,
    default_agent: usize,
    default_pty: usize,
    set: &mut std::collections::HashSet<(usize, usize)>,
) {
    match node {
        RenderNode::HSplit { children, .. } | RenderNode::VSplit { children, .. } => {
            for child in children {
                collect_bindings_recursive(child, default_agent, default_pty, set);
            }
        }
        RenderNode::Centered { child, .. } => {
            collect_bindings_recursive(child, default_agent, default_pty, set);
        }
        RenderNode::Widget {
            widget_type,
            props,
            ..
        } => {
            if matches!(widget_type, WidgetType::Terminal) {
                let (agent_idx, pty_idx) = match props {
                    Some(WidgetProps::Terminal(b)) => (
                        b.agent_index.unwrap_or(default_agent),
                        b.pty_index.unwrap_or(default_pty),
                    ),
                    _ => (default_agent, default_pty),
                };
                set.insert((agent_idx, pty_idx));
            }
        }
    }
}

// =============================================================================
// List Action Extraction
// =============================================================================

/// Extract action strings from the first list widget found in a render tree.
///
/// Walks the tree depth-first, finds the first `List` widget, and returns
/// the `action` strings for selectable (non-header) items in order.
/// Used to cache menu actions after rendering so Rust can map selection
/// index → action without rebuilding the menu.
pub fn extract_list_actions(node: &RenderNode) -> Vec<String> {
    let mut actions = Vec::new();
    extract_list_actions_recursive(node, &mut actions);
    actions
}

fn extract_list_actions_recursive(node: &RenderNode, actions: &mut Vec<String>) -> bool {
    match node {
        RenderNode::HSplit { children, .. } | RenderNode::VSplit { children, .. } => {
            for child in children {
                if extract_list_actions_recursive(child, actions) {
                    return true;
                }
            }
        }
        RenderNode::Centered { child, .. } => {
            return extract_list_actions_recursive(child, actions);
        }
        RenderNode::Widget {
            widget_type,
            props,
            ..
        } => {
            if matches!(widget_type, WidgetType::List) {
                if let Some(WidgetProps::List(list_props)) = props {
                    for item in &list_props.items {
                        if !item.header {
                            actions.push(item.action.clone().unwrap_or_default());
                        }
                    }
                }
                return true; // Stop after first list
            }
        }
    }
    false
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // === Constraint Parsing ===

    #[test]
    fn test_parse_percentage_constraint() {
        assert_eq!(parse_constraint("30%").unwrap(), Constraint::Percentage(30));
        assert_eq!(
            parse_constraint("100%").unwrap(),
            Constraint::Percentage(100)
        );
    }

    #[test]
    fn test_parse_length_constraint() {
        assert_eq!(parse_constraint("20").unwrap(), Constraint::Length(20));
        assert_eq!(parse_constraint("0").unwrap(), Constraint::Length(0));
    }

    #[test]
    fn test_parse_min_max_constraint() {
        assert_eq!(parse_constraint("min:10").unwrap(), Constraint::Min(10));
        assert_eq!(parse_constraint("max:80").unwrap(), Constraint::Max(80));
    }

    #[test]
    fn test_parse_constraint_with_whitespace() {
        assert_eq!(
            parse_constraint("  30%  ").unwrap(),
            Constraint::Percentage(30)
        );
    }

    #[test]
    fn test_parse_invalid_constraint() {
        assert!(parse_constraint("abc").is_err());
        assert!(parse_constraint("%30").is_err());
    }

    // === Lua Table Deserialization ===

    #[test]
    fn test_parse_simple_widget() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            return { type = "empty", block = { title = " Agents ", borders = "all" } }
        "#,
        )
        .eval::<LuaTable>()
        .and_then(|table| {
            let node = RenderNode::from_lua_table(&table).unwrap();
            match node {
                RenderNode::Widget { widget_type, block, .. } => {
                    assert!(matches!(widget_type, WidgetType::Empty));
                    assert!(block.is_some());
                    let block = block.unwrap();
                    assert_eq!(block.title.as_ref().and_then(|t| t.as_plain_str()), Some(" Agents "));
                    assert!(matches!(block.borders, BorderStyle::All));
                }
                _ => panic!("Expected Widget node"),
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn test_parse_hsplit() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "hsplit",
                constraints = { "30%", "70%" },
                children = {
                    { type = "empty" },
                    { type = "terminal" },
                }
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::HSplit {
                constraints,
                children,
            } => {
                assert_eq!(constraints.len(), 2);
                assert_eq!(constraints[0], Constraint::Percentage(30));
                assert_eq!(constraints[1], Constraint::Percentage(70));
                assert_eq!(children.len(), 2);
            }
            _ => panic!("Expected HSplit node"),
        }
    }

    #[test]
    fn test_parse_centered() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "centered",
                width = 50,
                height = 40,
                child = { type = "empty", block = { title = " Modal ", borders = "all" } }
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::Centered {
                width_pct,
                height_pct,
                child,
            } => {
                assert_eq!(width_pct, 50);
                assert_eq!(height_pct, 40);
                assert!(matches!(*child, RenderNode::Widget { .. }));
            }
            _ => panic!("Expected Centered node"),
        }
    }

    #[test]
    fn test_parse_mismatched_constraints_children() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "hsplit",
                constraints = { "30%", "70%" },
                children = {
                    { type = "empty" },
                }
            }
        "#,
            )
            .eval()
            .unwrap();

        let result = RenderNode::from_lua_table(&table);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("2 constraints but 1 children"));
    }

    #[test]
    fn test_parse_unknown_widget_type() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(r#"return { type = "nonexistent_widget" }"#)
            .eval()
            .unwrap();

        let result = RenderNode::from_lua_table(&table);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_paragraph_with_lines() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "paragraph",
                props = { lines = { "Line 1", "Line 2", "Line 3" } },
                block = { title = " Info ", borders = "all" }
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::Widget {
                widget_type,
                props,
                ..
            } => {
                assert!(matches!(widget_type, WidgetType::Paragraph));
                let WidgetProps::Paragraph(para) = props.expect("should have props") else {
                    panic!("Expected Paragraph props");
                };
                assert_eq!(para.lines.len(), 3);
                assert_eq!(para.lines[0], "Line 1");
            }
            _ => panic!("Expected Widget node"),
        }
    }

    #[test]
    fn test_parse_list_widget() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "list",
                block = { title = " Items ", borders = "all" },
                props = {
                    items = {
                        "plain item",
                        { text = "Header", header = true },
                        { text = "styled", style = { fg = "cyan" } },
                    },
                    selected = 1,
                    highlight_symbol = ">> ",
                },
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::Widget {
                widget_type,
                props,
                ..
            } => {
                assert!(matches!(widget_type, WidgetType::List));
                let WidgetProps::List(list) = props.expect("should have props") else {
                    panic!("Expected List props");
                };
                assert_eq!(list.items.len(), 3);
                assert!(!list.items[0].header);
                assert!(list.items[1].header);
                assert_eq!(list.selected, Some(1));
                assert_eq!(list.highlight_symbol.as_deref(), Some(">> "));
            }
            _ => panic!("Expected Widget node"),
        }
    }

    #[test]
    fn test_parse_input_widget() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "input",
                block = { title = " Enter name ", borders = "all" },
                props = {
                    lines = { "Type a name:" },
                    value = "hello",
                },
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::Widget {
                widget_type,
                props,
                ..
            } => {
                assert!(matches!(widget_type, WidgetType::Input));
                let WidgetProps::Input(input) = props.expect("should have props") else {
                    panic!("Expected Input props");
                };
                assert_eq!(input.lines.len(), 1);
                assert_eq!(input.value, "hello");
            }
            _ => panic!("Expected Widget node"),
        }
    }

    #[test]
    fn test_parse_widget_without_block() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(r#"return { type = "terminal" }"#)
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::Widget { block, .. } => {
                assert!(block.is_none());
            }
            _ => panic!("Expected Widget node"),
        }
    }

    // === Block Config ===

    #[test]
    fn test_block_config_to_block() {
        let config = BlockConfig {
            title: Some(StyledContent::Plain(" Test ".to_string())),
            borders: BorderStyle::All,
            border_style: None,
        };
        // Just verify it doesn't panic — Block doesn't implement PartialEq
        let _block = config.to_block();
    }

    #[test]
    fn test_block_config_no_borders() {
        let config = BlockConfig {
            title: None,
            borders: BorderStyle::None,
            border_style: None,
        };
        let _block = config.to_block();
    }

    #[test]
    fn test_block_config_with_border_style() {
        let config = BlockConfig {
            title: Some(StyledContent::Plain(" Focused ".to_string())),
            borders: BorderStyle::All,
            border_style: Some(SpanStyle {
                fg: Some(SpanColor::Cyan),
                ..SpanStyle::default()
            }),
        };
        let _block = config.to_block();
    }

    // === Nested Tree Parsing ===

    #[test]
    fn test_parse_nested_tree() {
        let lua = mlua::Lua::new();
        let table: LuaTable = lua
            .load(
                r#"
            return {
                type = "vsplit",
                constraints = { "90%", "10%" },
                children = {
                    {
                        type = "hsplit",
                        constraints = { "30%", "70%" },
                        children = {
                            { type = "empty", block = { title = " Agents ", borders = "all" } },
                            { type = "terminal", block = { title = " Terminal ", borders = "all" } },
                        }
                    },
                    { type = "empty", block = { title = " Status ", borders = "all" } },
                }
            }
        "#,
            )
            .eval()
            .unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match node {
            RenderNode::VSplit {
                constraints,
                children,
            } => {
                assert_eq!(constraints.len(), 2);
                assert_eq!(children.len(), 2);
                // First child should be an HSplit
                assert!(matches!(children[0], RenderNode::HSplit { .. }));
                // Second child should be a Widget
                assert!(matches!(children[1], RenderNode::Widget { .. }));
            }
            _ => panic!("Expected VSplit node"),
        }
    }

    // === Widget Type Parsing ===

    #[test]
    fn test_parse_all_widget_types() {
        let lua = mlua::Lua::new();
        let types = [
            ("terminal", "Terminal"),
            ("connection_code", "ConnectionCode"),
            ("empty", "Empty"),
        ];

        for (type_str, label) in &types {
            let table = lua.create_table().unwrap();
            table.set("type", *type_str).unwrap();
            let node = RenderNode::from_lua_table(&table).unwrap();
            match &node {
                RenderNode::Widget { widget_type, block, .. } => {
                    assert!(block.is_none(), "{label} should have no block");
                    let dbg = format!("{widget_type:?}");
                    assert!(dbg.contains(label), "Expected {label} in {dbg}");
                }
                _ => panic!("Expected Widget node for {label}"),
            }
        }
    }

    #[test]
    fn test_parse_centered_modal_overlay() {
        let lua = mlua::Lua::new();

        let child = lua.create_table().unwrap();
        child.set("type", "list").unwrap();
        let block_t = lua.create_table().unwrap();
        block_t.set("title", " Menu ").unwrap();
        block_t.set("borders", "all").unwrap();
        child.set("block", block_t).unwrap();

        let table = lua.create_table().unwrap();
        table.set("type", "centered").unwrap();
        table.set("width", 50u16).unwrap();
        table.set("height", 40u16).unwrap();
        table.set("child", child).unwrap();

        let node = RenderNode::from_lua_table(&table).unwrap();
        match &node {
            RenderNode::Centered {
                width_pct,
                height_pct,
                child,
            } => {
                assert_eq!(*width_pct, 50);
                assert_eq!(*height_pct, 40);
                match child.as_ref() {
                    RenderNode::Widget { widget_type, block, .. } => {
                        assert!(matches!(widget_type, WidgetType::List));
                        let b = block.as_ref().unwrap();
                        assert_eq!(b.title.as_ref().and_then(|t| t.as_plain_str()), Some(" Menu "));
                    }
                    _ => panic!("Expected Widget child"),
                }
            }
            _ => panic!("Expected Centered node"),
        }
    }

    // === Terminal Binding Collection ===

    #[test]
    fn test_collect_bindings_single_terminal_no_props() {
        let tree = RenderNode::Widget {
            widget_type: WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: None,
        };
        let bindings = collect_terminal_bindings(&tree, 2, 1);
        assert_eq!(bindings.len(), 1);
        assert!(bindings.contains(&(2, 1)), "No-props terminal should use defaults");
    }

    #[test]
    fn test_collect_bindings_explicit_props() {
        let tree = RenderNode::Widget {
            widget_type: WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: Some(WidgetProps::Terminal(TerminalBinding {
                agent_index: Some(0),
                pty_index: Some(1),
            })),
        };
        let bindings = collect_terminal_bindings(&tree, 2, 0);
        assert_eq!(bindings.len(), 1);
        assert!(bindings.contains(&(0, 1)));
    }

    #[test]
    fn test_collect_bindings_partial_props() {
        let tree = RenderNode::Widget {
            widget_type: WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: Some(WidgetProps::Terminal(TerminalBinding {
                agent_index: None,
                pty_index: Some(1),
            })),
        };
        let bindings = collect_terminal_bindings(&tree, 3, 0);
        assert_eq!(bindings.len(), 1);
        assert!(bindings.contains(&(3, 1)), "agent_index should default to 3");
    }

    #[test]
    fn test_collect_bindings_multiple_terminals() {
        let tree = RenderNode::HSplit {
            constraints: vec![
                ratatui::layout::Constraint::Percentage(50),
                ratatui::layout::Constraint::Percentage(50),
            ],
            children: vec![
                RenderNode::Widget {
                    widget_type: WidgetType::Terminal,
                    block: None,
                    custom_lines: None,
                    props: Some(WidgetProps::Terminal(TerminalBinding {
                        agent_index: Some(0),
                        pty_index: Some(0),
                    })),
                },
                RenderNode::Widget {
                    widget_type: WidgetType::Terminal,
                    block: None,
                    custom_lines: None,
                    props: Some(WidgetProps::Terminal(TerminalBinding {
                        agent_index: Some(0),
                        pty_index: Some(1),
                    })),
                },
            ],
        };
        let bindings = collect_terminal_bindings(&tree, 0, 0);
        assert_eq!(bindings.len(), 2);
        assert!(bindings.contains(&(0, 0)));
        assert!(bindings.contains(&(0, 1)));
    }

    #[test]
    fn test_collect_bindings_non_terminal_widgets_ignored() {
        let tree = RenderNode::HSplit {
            constraints: vec![
                ratatui::layout::Constraint::Percentage(30),
                ratatui::layout::Constraint::Percentage(70),
            ],
            children: vec![
                RenderNode::Widget {
                    widget_type: WidgetType::List,
                    block: None,
                    custom_lines: None,
                    props: None,
                },
                RenderNode::Widget {
                    widget_type: WidgetType::Terminal,
                    block: None,
                    custom_lines: None,
                    props: None,
                },
            ],
        };
        let bindings = collect_terminal_bindings(&tree, 0, 0);
        assert_eq!(bindings.len(), 1, "Only terminal widgets should produce bindings");
        assert!(bindings.contains(&(0, 0)));
    }

    #[test]
    fn test_collect_bindings_nested_in_centered() {
        let tree = RenderNode::Centered {
            width_pct: 80,
            height_pct: 80,
            child: Box::new(RenderNode::Widget {
                widget_type: WidgetType::Terminal,
                block: None,
                custom_lines: None,
                props: Some(WidgetProps::Terminal(TerminalBinding {
                    agent_index: Some(1),
                    pty_index: Some(0),
                })),
            }),
        };
        let bindings = collect_terminal_bindings(&tree, 0, 0);
        assert_eq!(bindings.len(), 1);
        assert!(bindings.contains(&(1, 0)));
    }

    #[test]
    fn test_collect_bindings_deduplicates() {
        // Two terminal widgets with the same binding should produce one entry
        let tree = RenderNode::HSplit {
            constraints: vec![
                ratatui::layout::Constraint::Percentage(50),
                ratatui::layout::Constraint::Percentage(50),
            ],
            children: vec![
                RenderNode::Widget {
                    widget_type: WidgetType::Terminal,
                    block: None,
                    custom_lines: None,
                    props: Some(WidgetProps::Terminal(TerminalBinding {
                        agent_index: Some(0),
                        pty_index: Some(0),
                    })),
                },
                RenderNode::Widget {
                    widget_type: WidgetType::Terminal,
                    block: None,
                    custom_lines: None,
                    props: Some(WidgetProps::Terminal(TerminalBinding {
                        agent_index: Some(0),
                        pty_index: Some(0),
                    })),
                },
            ],
        };
        let bindings = collect_terminal_bindings(&tree, 0, 0);
        assert_eq!(bindings.len(), 1, "Duplicate bindings should be deduplicated");
    }
}
