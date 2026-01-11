package tui

import (
	"testing"

	"github.com/trybotster/botster-hub/internal/tunnel"
)

// =============================================================================
// AppMode Tests
// =============================================================================

func TestAppModeString(t *testing.T) {
	tests := []struct {
		mode     AppMode
		expected string
	}{
		{ModeNormal, "normal"},
		{ModeMenu, "menu"},
		{ModeNewAgentSelectWorktree, "select_worktree"},
		{ModeNewAgentCreateWorktree, "create_worktree"},
		{ModeNewAgentPrompt, "prompt"},
		{ModeCloseAgentConfirm, "close_confirm"},
		{ModeConnectionCode, "connection_code"},
		{AppMode(99), "normal"}, // Unknown defaults to normal
	}

	for _, tt := range tests {
		t.Run(tt.expected, func(t *testing.T) {
			if got := tt.mode.String(); got != tt.expected {
				t.Errorf("AppMode.String() = %q, want %q", got, tt.expected)
			}
		})
	}
}

// =============================================================================
// ViewState Tests
// =============================================================================

func TestNewViewState(t *testing.T) {
	vs := NewViewState()

	if vs == nil {
		t.Fatal("NewViewState returned nil")
	}
	if vs.Mode != ModeNormal {
		t.Errorf("Mode = %v, want ModeNormal", vs.Mode)
	}
	if !vs.PollingEnabled {
		t.Error("PollingEnabled should be true by default")
	}
	if vs.PollInterval != 10 {
		t.Errorf("PollInterval = %d, want 10", vs.PollInterval)
	}
	if vs.TunnelStatus != tunnel.StatusDisconnected {
		t.Errorf("TunnelStatus = %v, want Disconnected", vs.TunnelStatus)
	}
}

func TestViewStateSelectedKey(t *testing.T) {
	tests := []struct {
		name     string
		keys     []string
		selected int
		expected string
	}{
		{"empty keys", []string{}, 0, ""},
		{"valid selection", []string{"a", "b", "c"}, 1, "b"},
		{"first", []string{"x", "y"}, 0, "x"},
		{"last", []string{"x", "y"}, 1, "y"},
		{"out of bounds positive", []string{"a", "b"}, 5, ""},
		{"out of bounds negative", []string{"a", "b"}, -1, ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vs := &ViewState{AgentKeys: tt.keys, Selected: tt.selected}
			if got := vs.SelectedKey(); got != tt.expected {
				t.Errorf("SelectedKey() = %q, want %q", got, tt.expected)
			}
		})
	}
}

func TestViewStateHasAgents(t *testing.T) {
	tests := []struct {
		count    int
		expected bool
	}{
		{0, false},
		{1, true},
		{5, true},
	}

	for _, tt := range tests {
		vs := &ViewState{AgentCount: tt.count}
		if got := vs.HasAgents(); got != tt.expected {
			t.Errorf("HasAgents() with count=%d = %v, want %v", tt.count, got, tt.expected)
		}
	}
}

func TestViewStateIsModal(t *testing.T) {
	tests := []struct {
		mode     AppMode
		expected bool
	}{
		{ModeNormal, false},
		{ModeMenu, true},
		{ModeNewAgentSelectWorktree, true},
		{ModeNewAgentCreateWorktree, true},
		{ModeNewAgentPrompt, true},
		{ModeCloseAgentConfirm, true},
		{ModeConnectionCode, true},
	}

	for _, tt := range tests {
		vs := &ViewState{Mode: tt.mode}
		if got := vs.IsModal(); got != tt.expected {
			t.Errorf("IsModal() with mode=%v = %v, want %v", tt.mode, got, tt.expected)
		}
	}
}

// =============================================================================
// ViewContext Tests
// =============================================================================

func TestDefaultViewContext(t *testing.T) {
	vc := DefaultViewContext()

	if vc == nil {
		t.Fatal("DefaultViewContext returned nil")
	}
	if vc.Mode != ModeNormal {
		t.Errorf("Mode = %v, want ModeNormal", vc.Mode)
	}
	if !vc.PollingEnabled {
		t.Error("PollingEnabled should be true by default")
	}
	if vc.PollInterval != 10 {
		t.Errorf("PollInterval = %d, want 10", vc.PollInterval)
	}
	if vc.TunnelStatus != tunnel.StatusDisconnected {
		t.Errorf("TunnelStatus = %v, want Disconnected", vc.TunnelStatus)
	}
}

// =============================================================================
// AgentDisplayInfo Tests
// =============================================================================

func TestAgentDisplayInfoDisplayString(t *testing.T) {
	port3000 := uint16(3000)
	port8080 := uint16(8080)

	tests := []struct {
		name     string
		info     AgentDisplayInfo
		expected string
	}{
		{
			name:     "no tunnel port",
			info:     AgentDisplayInfo{Label: "agent-1"},
			expected: "agent-1",
		},
		{
			name: "with tunnel port, server not running",
			info: AgentDisplayInfo{
				Label:         "agent-2",
				TunnelPort:    &port3000,
				ServerRunning: false,
			},
			expected: "agent-2 ○:3000",
		},
		{
			name: "with tunnel port, server running",
			info: AgentDisplayInfo{
				Label:         "agent-3",
				TunnelPort:    &port8080,
				ServerRunning: true,
			},
			expected: "agent-3 ▶:8080",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.info.DisplayString(); got != tt.expected {
				t.Errorf("DisplayString() = %q, want %q", got, tt.expected)
			}
		})
	}
}

func TestFormatPort(t *testing.T) {
	tests := []struct {
		port     uint16
		expected string
	}{
		{0, "0"},
		{80, "80"},
		{443, "443"},
		{3000, "3000"},
		{8080, "8080"},
		{65535, "65535"},
	}

	for _, tt := range tests {
		if got := formatPort(tt.port); got != tt.expected {
			t.Errorf("formatPort(%d) = %q, want %q", tt.port, got, tt.expected)
		}
	}
}

// =============================================================================
// Poll/Tunnel Status Tests
// =============================================================================

func TestFormatPollStatus(t *testing.T) {
	tests := []struct {
		enabled          bool
		secondsSincePoll uint64
		expected         string
	}{
		{false, 0, "PAUSED"},
		{false, 100, "PAUSED"},
		{true, 0, "●"},
		{true, 1, "○"},
		{true, 10, "○"},
	}

	for _, tt := range tests {
		if got := FormatPollStatus(tt.enabled, tt.secondsSincePoll); got != tt.expected {
			t.Errorf("FormatPollStatus(%v, %d) = %q, want %q",
				tt.enabled, tt.secondsSincePoll, got, tt.expected)
		}
	}
}

func TestFormatTunnelStatus(t *testing.T) {
	tests := []struct {
		status   tunnel.Status
		expected string
	}{
		{tunnel.StatusDisconnected, "○"},
		{tunnel.StatusConnecting, "◐"},
		{tunnel.StatusConnected, "⬤"},
	}

	for _, tt := range tests {
		if got := FormatTunnelStatus(tt.status); got != tt.expected {
			t.Errorf("FormatTunnelStatus(%v) = %q, want %q", tt.status, got, tt.expected)
		}
	}
}

// =============================================================================
// Menu Tests
// =============================================================================

func TestBuildMenu(t *testing.T) {
	// Menu structure matches Rust: Agent section first (if agent), then Hub section
	tests := []struct {
		name           string
		hasAgent       bool
		hasServerPty   bool
		isServerView   bool
		pollingEnabled bool
		wantActions    []string
	}{
		{
			name:           "no agents, polling enabled",
			hasAgent:       false,
			hasServerPty:   false,
			isServerView:   false,
			pollingEnabled: true,
			wantActions:    []string{"new_agent", "connection_code", "toggle_polling"},
		},
		{
			name:           "with agent, no server pty",
			hasAgent:       true,
			hasServerPty:   false,
			isServerView:   false,
			pollingEnabled: true,
			wantActions:    []string{"close_agent", "new_agent", "connection_code", "toggle_polling"},
		},
		{
			name:           "with agent and server pty, cli view",
			hasAgent:       true,
			hasServerPty:   true,
			isServerView:   false,
			pollingEnabled: true,
			wantActions:    []string{"toggle_pty", "close_agent", "new_agent", "connection_code", "toggle_polling"},
		},
		{
			name:           "polling disabled",
			hasAgent:       false,
			hasServerPty:   false,
			isServerView:   false,
			pollingEnabled: false,
			wantActions:    []string{"new_agent", "connection_code", "toggle_polling"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			items := BuildMenu(tt.hasAgent, tt.hasServerPty, tt.isServerView, tt.pollingEnabled)

			// Extract non-header actions
			var actions []string
			for _, item := range items {
				if !item.IsHeader {
					actions = append(actions, item.Action)
				}
			}

			if len(actions) != len(tt.wantActions) {
				t.Errorf("got %d actions, want %d: %v vs %v", len(actions), len(tt.wantActions), actions, tt.wantActions)
				return
			}

			for i, action := range actions {
				if action != tt.wantActions[i] {
					t.Errorf("action[%d] = %q, want %q", i, action, tt.wantActions[i])
				}
			}
		})
	}
}

func TestBuildMenuPTYToggleLabel(t *testing.T) {
	// Test that the PTY toggle label changes based on current view
	cliView := BuildMenu(true, true, false, true)
	serverView := BuildMenu(true, true, true, true)

	var cliToggle, serverToggle string
	for _, item := range cliView {
		if item.Action == "toggle_pty" {
			cliToggle = item.Label
		}
	}
	for _, item := range serverView {
		if item.Action == "toggle_pty" {
			serverToggle = item.Label
		}
	}

	if cliToggle != "View Server" {
		t.Errorf("CLI view toggle label = %q, want 'View Server'", cliToggle)
	}
	if serverToggle != "View Agent" {
		t.Errorf("Server view toggle label = %q, want 'View Agent'", serverToggle)
	}
}

func TestBuildMenuPollingLabel(t *testing.T) {
	enabled := BuildMenu(false, false, false, true)
	disabled := BuildMenu(false, false, false, false)

	var enabledLabel, disabledLabel string
	for _, item := range enabled {
		if item.Action == "toggle_polling" {
			enabledLabel = item.Label
		}
	}
	for _, item := range disabled {
		if item.Action == "toggle_polling" {
			disabledLabel = item.Label
		}
	}

	if enabledLabel != "Toggle Polling (ON)" {
		t.Errorf("Enabled polling label = %q, want 'Toggle Polling (ON)'", enabledLabel)
	}
	if disabledLabel != "Toggle Polling (OFF)" {
		t.Errorf("Disabled polling label = %q, want 'Toggle Polling (OFF)'", disabledLabel)
	}
}

func TestSelectableCount(t *testing.T) {
	items := []MenuItem{
		{Label: "Header 1", IsHeader: true},
		{Label: "Item 1", Action: "action1"},
		{Label: "Item 2", Action: "action2"},
		{Label: "Header 2", IsHeader: true},
		{Label: "Item 3", Action: "action3"},
	}

	if got := SelectableCount(items); got != 3 {
		t.Errorf("SelectableCount() = %d, want 3", got)
	}

	// Empty list
	if got := SelectableCount(nil); got != 0 {
		t.Errorf("SelectableCount(nil) = %d, want 0", got)
	}

	// All headers
	headers := []MenuItem{
		{Label: "H1", IsHeader: true},
		{Label: "H2", IsHeader: true},
	}
	if got := SelectableCount(headers); got != 0 {
		t.Errorf("SelectableCount(all headers) = %d, want 0", got)
	}
}
