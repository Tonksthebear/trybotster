//! Context-aware menu system for the Hub.
//!
//! This module provides a dynamic menu that adapts based on:
//! - Whether an agent is selected
//! - Which PTY view is active (CLI/Server)
//! - Whether the agent has a server PTY
//!
//! The menu is divided into two sections:
//! - **Agent section**: Actions related to the selected agent (only shown when agent exists)
//! - **Hub section**: Global hub actions (always shown)

// Rust guideline compliant 2025-01

use crate::PtyView;

/// Actions that can be triggered from the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Toggle between CLI and Server PTY views.
    TogglePtyView,
    /// Close the currently selected agent.
    CloseAgent,
    /// Create a new agent.
    NewAgent,
    /// Show the connection code/QR code for browser access.
    ShowConnectionCode,
    /// Toggle automatic polling for new messages.
    TogglePolling,
}

/// A menu item with its display text and action.
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// Display label for the menu item.
    pub label: String,
    /// Action to perform when selected.
    pub action: MenuAction,
    /// Whether this item is a section header (not selectable).
    pub is_header: bool,
}

/// Context needed to build the menu.
#[derive(Debug, Clone)]
pub struct MenuContext {
    /// Whether an agent is currently selected.
    pub has_agent: bool,
    /// Whether the selected agent has a server PTY.
    pub has_server_pty: bool,
    /// Current PTY view for the selected agent.
    pub active_pty: PtyView,
    /// Whether polling is enabled.
    pub polling_enabled: bool,
}

impl Default for MenuContext {
    fn default() -> Self {
        Self {
            has_agent: false,
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        }
    }
}

/// Build the menu items based on context.
///
/// Returns a vector of menu items. Items with `is_header: true` are
/// section headers and should not be selectable.
#[must_use]
pub fn build_menu(ctx: &MenuContext) -> Vec<MenuItem> {
    let mut items = Vec::new();

    // === Agent Section (only if agent is selected) ===
    if ctx.has_agent {
        items.push(MenuItem {
            label: "── Agent ──".to_string(),
            action: MenuAction::TogglePtyView, // Placeholder, headers aren't selectable
            is_header: true,
        });

        // Toggle PTY view (only if agent has server PTY)
        if ctx.has_server_pty {
            let label = match ctx.active_pty {
                PtyView::Cli => "View Server",
                PtyView::Server => "View Agent",
            };
            items.push(MenuItem {
                label: label.to_string(),
                action: MenuAction::TogglePtyView,
                is_header: false,
            });
        }

        items.push(MenuItem {
            label: "Close Agent".to_string(),
            action: MenuAction::CloseAgent,
            is_header: false,
        });
    }

    // === Hub Section (always shown) ===
    items.push(MenuItem {
        label: "── Hub ──".to_string(),
        action: MenuAction::NewAgent, // Placeholder, headers aren't selectable
        is_header: true,
    });

    items.push(MenuItem {
        label: "New Agent".to_string(),
        action: MenuAction::NewAgent,
        is_header: false,
    });

    items.push(MenuItem {
        label: "Show Connection Code".to_string(),
        action: MenuAction::ShowConnectionCode,
        is_header: false,
    });

    let polling_label = format!(
        "Toggle Polling ({})",
        if ctx.polling_enabled { "ON" } else { "OFF" }
    );
    items.push(MenuItem {
        label: polling_label,
        action: MenuAction::TogglePolling,
        is_header: false,
    });

    items
}

/// Get the number of selectable items in the menu.
#[must_use]
pub fn selectable_count(items: &[MenuItem]) -> usize {
    items.iter().filter(|item| !item.is_header).count()
}

/// Convert a selection index (0-based among selectable items) to the actual item index.
#[must_use]
pub fn selection_to_item_index(items: &[MenuItem], selection: usize) -> Option<usize> {
    let mut selectable_idx = 0;
    for (i, item) in items.iter().enumerate() {
        if !item.is_header {
            if selectable_idx == selection {
                return Some(i);
            }
            selectable_idx += 1;
        }
    }
    None
}

/// Get the action for a given selection index.
#[must_use]
pub fn get_action_for_selection(items: &[MenuItem], selection: usize) -> Option<MenuAction> {
    selection_to_item_index(items, selection).map(|idx| items[idx].action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_without_agent() {
        let ctx = MenuContext {
            has_agent: false,
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let items = build_menu(&ctx);

        // Should only have Hub section
        assert!(items.iter().any(|i| i.label.contains("Hub")));
        assert!(!items
            .iter()
            .any(|i| i.label.contains("Agent") && i.is_header));

        // Should have 3 selectable items (New Agent, Connection Code, Toggle Polling)
        assert_eq!(selectable_count(&items), 3);
    }

    #[test]
    fn test_menu_with_agent_no_server() {
        let ctx = MenuContext {
            has_agent: true,
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let items = build_menu(&ctx);

        // Should have both sections
        assert!(items
            .iter()
            .any(|i| i.label.contains("Agent") && i.is_header));
        assert!(items.iter().any(|i| i.label.contains("Hub")));

        // Should have Close Agent but NOT View Server
        assert!(items.iter().any(|i| i.label == "Close Agent"));
        assert!(!items.iter().any(|i| i.label == "View Server"));

        // 4 selectable items (Close Agent + 3 Hub items)
        assert_eq!(selectable_count(&items), 4);
    }

    #[test]
    fn test_menu_with_agent_and_server_on_cli() {
        let ctx = MenuContext {
            has_agent: true,
            has_server_pty: true,
            active_pty: PtyView::Cli,
            polling_enabled: false,
        };
        let items = build_menu(&ctx);

        // Should show "View Server" when on CLI
        assert!(items.iter().any(|i| i.label == "View Server"));
        assert!(!items.iter().any(|i| i.label == "View Agent"));

        // Polling should show OFF
        assert!(items.iter().any(|i| i.label.contains("OFF")));

        // 5 selectable items
        assert_eq!(selectable_count(&items), 5);
    }

    #[test]
    fn test_menu_with_agent_and_server_on_server() {
        let ctx = MenuContext {
            has_agent: true,
            has_server_pty: true,
            active_pty: PtyView::Server,
            polling_enabled: true,
        };
        let items = build_menu(&ctx);

        // Should show "View Agent" when on Server
        assert!(items.iter().any(|i| i.label == "View Agent"));
        assert!(!items.iter().any(|i| i.label == "View Server"));
    }

    #[test]
    fn test_selection_to_item_index() {
        let ctx = MenuContext {
            has_agent: true,
            has_server_pty: true,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let items = build_menu(&ctx);

        // Selection 0 should be first selectable (View Server), not the header
        let idx = selection_to_item_index(&items, 0).unwrap();
        assert!(!items[idx].is_header);
        assert_eq!(items[idx].label, "View Server");
    }

    #[test]
    fn test_get_action_for_selection() {
        let ctx = MenuContext {
            has_agent: true,
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let items = build_menu(&ctx);

        // First selectable should be Close Agent
        let action = get_action_for_selection(&items, 0);
        assert_eq!(action, Some(MenuAction::CloseAgent));

        // Second should be New Agent
        let action = get_action_for_selection(&items, 1);
        assert_eq!(action, Some(MenuAction::NewAgent));
    }
}
