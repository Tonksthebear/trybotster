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
        custom_lines: Option<Vec<String>>,
    },
}

/// Named widget types implemented in Rust.
///
/// Lua refers to these by string name. Each maps to existing rendering logic.
#[derive(Debug, Clone)]
pub enum WidgetType {
    /// Agent list panel with stateful selection.
    AgentList,
    /// Terminal panel showing vt100 PTY output.
    Terminal,
    /// Menu modal with selectable items.
    Menu,
    /// Worktree selection list for agent creation.
    WorktreeSelect,
    /// Text input field (worktree creation, agent prompt).
    TextInput,
    /// Close agent confirmation dialog (Y/D/N).
    CloseConfirm,
    /// QR code / connection code display.
    ConnectionCode,
    /// Error message display.
    Error,
    /// Empty placeholder or static text block.
    ///
    /// With no `custom_lines`, renders just the block border/title.
    /// With `custom_lines`, renders a paragraph of those lines inside the block.
    Empty,
}

/// Configuration for a ratatui Block (border + title).
#[derive(Debug, Clone)]
pub struct BlockConfig {
    /// Block title text.
    pub title: Option<String>,
    /// Border style.
    pub borders: BorderStyle,
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
    /// { type = "agent_list", block = { title = "Agents", borders = "all" } }
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
            "agent_list" => WidgetType::AgentList,
            "terminal" => WidgetType::Terminal,
            "menu" => WidgetType::Menu,
            "worktree_select" => WidgetType::WorktreeSelect,
            "text_input" => WidgetType::TextInput,
            "close_confirm" => WidgetType::CloseConfirm,
            "connection_code" => WidgetType::ConnectionCode,
            "error" => WidgetType::Error,
            "paragraph" | "empty" => WidgetType::Empty,
            _ => {
                return Err(anyhow!("Unknown widget type: '{type_name}'"));
            }
        };

        let block = parse_block_config(table);
        let custom_lines = parse_string_array(table, "lines").ok();

        Ok(RenderNode::Widget {
            widget_type,
            block,
            custom_lines,
        })
    }
}

/// Parse optional block config from a table.
fn parse_block_config(table: &LuaTable) -> Option<BlockConfig> {
    let block_value: LuaValue = table.get("block").ok()?;

    let LuaValue::Table(block_table) = block_value else {
        return None;
    };

    let title: Option<String> = block_table.get("title").ok();

    let borders_str: Option<String> = block_table.get("borders").ok();
    let borders = match borders_str.as_deref() {
        Some("none") => BorderStyle::None,
        _ => BorderStyle::All,
    };

    Some(BlockConfig { title, borders })
}

/// Parse an array of strings from a table field.
fn parse_string_array(table: &LuaTable, key: &str) -> Result<Vec<String>> {
    let arr: LuaTable = table
        .get(key)
        .map_err(|e| anyhow!("Missing array field '{key}': {e}"))?;

    let mut result = Vec::new();
    for val in arr.sequence_values::<String>() {
        result.push(val.map_err(|e| anyhow!("Invalid string in '{key}': {e}"))?);
    }
    Ok(result)
}

// =============================================================================
// Block Config → ratatui Block
// =============================================================================

impl BlockConfig {
    /// Convert to a ratatui `Block` widget.
    #[must_use]
    pub fn to_block(&self) -> Block<'_> {
        let mut block = Block::default();

        match self.borders {
            BorderStyle::All => {
                block = block.borders(Borders::ALL);
            }
            BorderStyle::None => {}
        }

        if let Some(ref title) = self.title {
            block = block.title(title.as_str());
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
        } => {
            render_widget(widget_type, block.as_ref(), custom_lines.as_deref(), f, ctx, area);
        }
    }
}

/// Render a leaf widget using existing Rust rendering functions.
fn render_widget(
    widget_type: &WidgetType,
    block_cfg: Option<&BlockConfig>,
    custom_lines: Option<&[String]>,
    f: &mut Frame,
    ctx: &RenderContext,
    area: Rect,
) {
    let block = block_cfg.map(BlockConfig::to_block).unwrap_or_default();

    match widget_type {
        WidgetType::AgentList => {
            super::render::render_agent_list(f, ctx, area);
        }
        WidgetType::Terminal => {
            super::render::render_terminal_panel(f, ctx, area);
        }
        WidgetType::Menu => {
            super::render::render_menu_widget(f, ctx, area, block);
        }
        WidgetType::WorktreeSelect => {
            super::render::render_worktree_select_widget(f, ctx, area, block);
        }
        WidgetType::TextInput => {
            super::render::render_text_input_widget(f, ctx, area, block, custom_lines);
        }
        WidgetType::CloseConfirm => {
            super::render::render_close_confirm_widget(f, area, block, custom_lines);
        }
        WidgetType::ConnectionCode => {
            super::render::render_connection_code_widget(f, ctx, area, block, custom_lines);
        }
        WidgetType::Error => {
            super::render::render_error_widget(f, ctx, area, block, custom_lines);
        }
        WidgetType::Empty => {
            if let Some(lines) = custom_lines {
                let text: Vec<ratatui::text::Line> =
                    lines.iter().map(|l| ratatui::text::Line::from(l.as_str())).collect();
                let paragraph = ratatui::widgets::Paragraph::new(text)
                    .block(block)
                    .alignment(ratatui::layout::Alignment::Left)
                    .wrap(ratatui::widgets::Wrap { trim: false });
                f.render_widget(paragraph, area);
            } else {
                f.render_widget(block, area);
            }
        }
    }
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
            return { type = "agent_list", block = { title = " Agents ", borders = "all" } }
        "#,
        )
        .eval::<LuaTable>()
        .and_then(|table| {
            let node = RenderNode::from_lua_table(&table).unwrap();
            match node {
                RenderNode::Widget { widget_type, block, .. } => {
                    assert!(matches!(widget_type, WidgetType::AgentList));
                    assert!(block.is_some());
                    let block = block.unwrap();
                    assert_eq!(block.title.as_deref(), Some(" Agents "));
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
                    { type = "agent_list" },
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
                    { type = "agent_list" },
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
                lines = { "Line 1", "Line 2", "Line 3" },
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
                custom_lines,
                ..
            } => {
                assert!(matches!(widget_type, WidgetType::Empty));
                let lines = custom_lines.expect("paragraph should have custom_lines");
                assert_eq!(lines.len(), 3);
                assert_eq!(lines[0], "Line 1");
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
            title: Some(" Test ".to_string()),
            borders: BorderStyle::All,
        };
        // Just verify it doesn't panic — Block doesn't implement PartialEq
        let _block = config.to_block();
    }

    #[test]
    fn test_block_config_no_borders() {
        let config = BlockConfig {
            title: None,
            borders: BorderStyle::None,
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
                            { type = "agent_list", block = { title = " Agents ", borders = "all" } },
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

    // === Modal Widget Type Parsing ===

    #[test]
    fn test_parse_modal_widget_types() {
        let lua = mlua::Lua::new();
        let types = [
            ("worktree_select", "WorktreeSelect"),
            ("text_input", "TextInput"),
            ("close_confirm", "CloseConfirm"),
            ("connection_code", "ConnectionCode"),
            ("error", "Error"),
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
        child.set("type", "menu").unwrap();
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
                        assert!(matches!(widget_type, WidgetType::Menu));
                        let b = block.as_ref().unwrap();
                        assert_eq!(b.title.as_deref(), Some(" Menu "));
                    }
                    _ => panic!("Expected Widget child"),
                }
            }
            _ => panic!("Expected Centered node"),
        }
    }
}
