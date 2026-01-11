// Package tui provides the terminal user interface for botster-hub.
//
// This package acts as an adapter between the Hub (business logic) and the
// local terminal, using Bubble Tea for rendering and input handling.
package tui

import "github.com/trybotster/botster-hub/internal/tunnel"

// AppMode represents the current application mode.
type AppMode int

const (
	ModeNormal AppMode = iota
	ModeMenu
	ModeNewAgentSelectWorktree
	ModeNewAgentCreateWorktree
	ModeNewAgentPrompt
	ModeCloseAgentConfirm
	ModeConnectionCode
)

func (m AppMode) String() string {
	switch m {
	case ModeMenu:
		return "menu"
	case ModeNewAgentSelectWorktree:
		return "select_worktree"
	case ModeNewAgentCreateWorktree:
		return "create_worktree"
	case ModeNewAgentPrompt:
		return "prompt"
	case ModeCloseAgentConfirm:
		return "close_confirm"
	case ModeConnectionCode:
		return "connection_code"
	default:
		return "normal"
	}
}

// ViewState is a snapshot of state needed for TUI rendering.
type ViewState struct {
	// Number of active agents.
	AgentCount int
	// Ordered list of agent session keys.
	AgentKeys []string
	// Currently selected agent index.
	Selected int
	// Current application mode.
	Mode AppMode
	// Whether server polling is enabled.
	PollingEnabled bool
	// Seconds since last poll.
	SecondsSincePoll uint64
	// Poll interval in seconds.
	PollInterval uint64
	// Currently selected menu item.
	MenuSelected int
	// Available worktrees for selection.
	AvailableWorktrees []WorktreeEntry
	// Currently selected worktree index.
	WorktreeSelected int
	// Current text input buffer.
	InputBuffer string
	// Tunnel connection status.
	TunnelStatus tunnel.Status
	// Connection URL for QR code display.
	ConnectionURL string
}

// WorktreeEntry represents a worktree path and branch.
type WorktreeEntry struct {
	Path   string
	Branch string
}

// NewViewState creates a new ViewState with default values.
func NewViewState() *ViewState {
	return &ViewState{
		Mode:           ModeNormal,
		PollingEnabled: true,
		PollInterval:   10,
		TunnelStatus:   tunnel.StatusDisconnected,
	}
}

// SelectedKey returns the session key of the currently selected agent.
func (v *ViewState) SelectedKey() string {
	if v.Selected >= 0 && v.Selected < len(v.AgentKeys) {
		return v.AgentKeys[v.Selected]
	}
	return ""
}

// HasAgents returns true if there are any active agents.
func (v *ViewState) HasAgents() bool {
	return v.AgentCount > 0
}

// IsModal returns true if currently in a modal state.
func (v *ViewState) IsModal() bool {
	return v.Mode != ModeNormal
}

// ViewContext contains additional context needed for view rendering.
type ViewContext struct {
	// Current application mode.
	Mode AppMode
	// Whether polling is enabled.
	PollingEnabled bool
	// Seconds since last poll.
	SecondsSincePoll uint64
	// Poll interval configuration.
	PollInterval uint64
	// Currently selected menu item.
	MenuSelected int
	// Currently selected worktree.
	WorktreeSelected int
	// Text input buffer.
	InputBuffer string
	// Tunnel status.
	TunnelStatus tunnel.Status
	// Connection URL for QR code.
	ConnectionURL string
}

// DefaultViewContext returns a ViewContext with default values.
func DefaultViewContext() *ViewContext {
	return &ViewContext{
		Mode:           ModeNormal,
		PollingEnabled: true,
		PollInterval:   10,
		TunnelStatus:   tunnel.StatusDisconnected,
	}
}

// AgentDisplayInfo contains information about an agent for display.
type AgentDisplayInfo struct {
	// Display label for the agent.
	Label string
	// Session key for the agent.
	SessionKey string
	// Tunnel port if assigned.
	TunnelPort *uint16
	// Whether the server is running.
	ServerRunning bool
	// Which PTY view is active ("cli" or "server").
	ActiveView string
	// Whether the agent is scrolled up.
	IsScrolled bool
}

// DisplayString returns the full display string with server status.
func (a *AgentDisplayInfo) DisplayString() string {
	if a.TunnelPort != nil {
		icon := "○"
		if a.ServerRunning {
			icon = "▶"
		}
		return a.Label + " " + icon + ":" + formatPort(*a.TunnelPort)
	}
	return a.Label
}

func formatPort(port uint16) string {
	if port == 0 {
		return "0"
	}
	// Simple port to string conversion
	digits := make([]byte, 0, 5)
	for port > 0 {
		digits = append([]byte{byte('0' + port%10)}, digits...)
		port /= 10
	}
	return string(digits)
}

// FormatPollStatus formats the poll status indicator.
func FormatPollStatus(enabled bool, secondsSincePoll uint64) string {
	if !enabled {
		return "PAUSED"
	}
	if secondsSincePoll < 1 {
		return "●"
	}
	return "○"
}

// FormatTunnelStatus formats the tunnel status indicator.
func FormatTunnelStatus(status tunnel.Status) string {
	switch status {
	case tunnel.StatusConnected:
		return "⬤"
	case tunnel.StatusConnecting:
		return "◐"
	default:
		return "○"
	}
}

// VPNStatus represents VPN connection state.
type VPNStatus int

const (
	VPNStatusDisabled VPNStatus = iota
	VPNStatusDisconnected
	VPNStatusConnecting
	VPNStatusConnected
	VPNStatusError
)

// FormatVPNStatus formats the VPN status indicator.
// Matches Rust: ⬤ (connected), ◐ (connecting), ✕ (error), ○ (disconnected), - (disabled)
func FormatVPNStatus(status VPNStatus) string {
	switch status {
	case VPNStatusConnected:
		return "⬤"
	case VPNStatusConnecting:
		return "◐"
	case VPNStatusError:
		return "✕"
	case VPNStatusDisconnected:
		return "○"
	default: // VPNStatusDisabled
		return "-"
	}
}

// MenuItem represents a menu item.
type MenuItem struct {
	Label    string
	Action   string
	IsHeader bool
}

// BuildMenu builds the menu items based on current context.
// Matches Rust structure: Agent section first (if agent selected), then Hub section.
func BuildMenu(hasAgent, hasServerPty, isServerView, pollingEnabled bool) []MenuItem {
	var items []MenuItem

	// Agent section comes FIRST if an agent is selected (matches Rust)
	if hasAgent {
		items = append(items, MenuItem{Label: "── Agent ──", IsHeader: true})

		// PTY toggle is first item in agent section (if available)
		if hasServerPty {
			if isServerView {
				items = append(items, MenuItem{Label: "View Agent", Action: "toggle_pty"})
			} else {
				items = append(items, MenuItem{Label: "View Server", Action: "toggle_pty"})
			}
		}

		items = append(items, MenuItem{Label: "Close Agent", Action: "close_agent"})
	}

	// Hub section
	items = append(items, MenuItem{Label: "── Hub ──", IsHeader: true})
	items = append(items, MenuItem{Label: "New Agent", Action: "new_agent"})
	items = append(items, MenuItem{Label: "Show Connection Code", Action: "connection_code"})

	// Polling toggle
	if pollingEnabled {
		items = append(items, MenuItem{Label: "Toggle Polling (ON)", Action: "toggle_polling"})
	} else {
		items = append(items, MenuItem{Label: "Toggle Polling (OFF)", Action: "toggle_polling"})
	}

	return items
}

// SelectableCount returns the number of selectable (non-header) menu items.
func SelectableCount(items []MenuItem) int {
	count := 0
	for _, item := range items {
		if !item.IsHeader {
			count++
		}
	}
	return count
}
