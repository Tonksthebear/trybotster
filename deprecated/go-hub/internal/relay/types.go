// Package relay provides browser connectivity via Tailscale mesh.
//
// This module handles browser relay functionality via Tailscale/Headscale
// mesh networking. Browser connects to CLI via Tailscale SSH.
package relay

import "encoding/json"

// TerminalMessage types for CLI → Browser communication.
type TerminalMessage struct {
	Type       string         `json:"type"`
	Data       string         `json:"data,omitempty"`
	Agents     []AgentInfo    `json:"agents,omitempty"`
	Worktrees  []WorktreeInfo `json:"worktrees,omitempty"`
	Repo       string         `json:"repo,omitempty"`
	ID         string         `json:"id,omitempty"`
	Message    string         `json:"message,omitempty"`
	Lines      []string       `json:"lines,omitempty"`
}

// OutputMessage creates a terminal output message.
func OutputMessage(data string) TerminalMessage {
	return TerminalMessage{Type: "output", Data: data}
}

// AgentsMessage creates an agents list message.
func AgentsMessage(agents []AgentInfo) TerminalMessage {
	return TerminalMessage{Type: "agents", Agents: agents}
}

// WorktreesMessage creates a worktrees list message.
func WorktreesMessage(worktrees []WorktreeInfo, repo string) TerminalMessage {
	return TerminalMessage{Type: "worktrees", Worktrees: worktrees, Repo: repo}
}

// AgentSelectedMessage creates an agent selected message.
func AgentSelectedMessage(id string) TerminalMessage {
	return TerminalMessage{Type: "agent_selected", ID: id}
}

// AgentCreatedMessage creates an agent created message.
func AgentCreatedMessage(id string) TerminalMessage {
	return TerminalMessage{Type: "agent_created", ID: id}
}

// AgentDeletedMessage creates an agent deleted message.
func AgentDeletedMessage(id string) TerminalMessage {
	return TerminalMessage{Type: "agent_deleted", ID: id}
}

// ErrorMessage creates an error message.
func ErrorMessage(msg string) TerminalMessage {
	return TerminalMessage{Type: "error", Message: msg}
}

// ScrollbackMessage creates a scrollback message.
func ScrollbackMessage(lines []string) TerminalMessage {
	return TerminalMessage{Type: "scrollback", Lines: lines}
}

// AgentInfo contains agent details for browser display.
type AgentInfo struct {
	ID            string  `json:"id"`
	Repo          *string `json:"repo,omitempty"`
	IssueNumber   *uint64 `json:"issue_number,omitempty"`
	BranchName    *string `json:"branch_name,omitempty"`
	Name          *string `json:"name,omitempty"`
	Status        *string `json:"status,omitempty"`
	TunnelPort    *uint16 `json:"tunnel_port,omitempty"`
	ServerRunning *bool   `json:"server_running,omitempty"`
	HasServerPty  *bool   `json:"has_server_pty,omitempty"`
	ActivePtyView *string `json:"active_pty_view,omitempty"`
	ScrollOffset  *uint32 `json:"scroll_offset,omitempty"`
	HubIdentifier *string `json:"hub_identifier,omitempty"`
}

// WorktreeInfo contains worktree details for browser display.
type WorktreeInfo struct {
	Path        string  `json:"path"`
	Branch      string  `json:"branch"`
	IssueNumber *uint64 `json:"issue_number,omitempty"`
}

// BrowserCommand types for Browser → CLI communication.
type BrowserCommand struct {
	Type              string  `json:"type"`
	Data              string  `json:"data,omitempty"`
	Mode              string  `json:"mode,omitempty"`
	ID                string  `json:"id,omitempty"`
	IssueOrBranch     *string `json:"issue_or_branch,omitempty"`
	Prompt            *string `json:"prompt,omitempty"`
	Path              string  `json:"path,omitempty"`
	Branch            string  `json:"branch,omitempty"`
	DeleteWorktree    *bool   `json:"delete_worktree,omitempty"`
	Direction         string  `json:"direction,omitempty"`
	Lines             *uint32 `json:"lines,omitempty"`
	Cols              uint16  `json:"cols,omitempty"`
	Rows              uint16  `json:"rows,omitempty"`
	DeviceName        string  `json:"device_name,omitempty"`
	BrowserCurve25519 string  `json:"browser_curve25519,omitempty"`
}

// ParseBrowserCommand parses a JSON string into a BrowserCommand.
func ParseBrowserCommand(data []byte) (*BrowserCommand, error) {
	var cmd BrowserCommand
	if err := json.Unmarshal(data, &cmd); err != nil {
		return nil, err
	}
	return &cmd, nil
}

// BrowserResize contains terminal dimensions from browser.
type BrowserResize struct {
	Cols uint16
	Rows uint16
}

// BrowserEvent represents parsed browser events for Hub consumption.
type BrowserEvent struct {
	Type           BrowserEventType
	PublicKey      string // For Connected
	DeviceName     string // For Connected
	Data           string // For Input
	Resize         *BrowserResize
	Mode           string         // For SetMode
	ID             string         // For SelectAgent, DeleteAgent
	IssueOrBranch  *string        // For CreateAgent
	Prompt         *string        // For CreateAgent, ReopenWorktree
	Path           string         // For ReopenWorktree
	Branch         string         // For ReopenWorktree
	DeleteWorktree bool           // For DeleteAgent
	Direction      string         // For Scroll
	Lines          uint32         // For Scroll
}

// BrowserEventType identifies the type of browser event.
type BrowserEventType int

const (
	EventConnected BrowserEventType = iota
	EventDisconnected
	EventInput
	EventResize
	EventSetMode
	EventListAgents
	EventListWorktrees
	EventSelectAgent
	EventCreateAgent
	EventReopenWorktree
	EventDeleteAgent
	EventTogglePtyView
	EventScroll
	EventScrollToBottom
	EventScrollToTop
)

// CommandToEvent converts a BrowserCommand to a BrowserEvent.
func CommandToEvent(cmd *BrowserCommand) BrowserEvent {
	switch cmd.Type {
	case "handshake":
		return BrowserEvent{
			Type:       EventConnected,
			PublicKey:  cmd.BrowserCurve25519,
			DeviceName: cmd.DeviceName,
		}
	case "input":
		return BrowserEvent{Type: EventInput, Data: cmd.Data}
	case "set_mode":
		return BrowserEvent{Type: EventSetMode, Mode: cmd.Mode}
	case "list_agents":
		return BrowserEvent{Type: EventListAgents}
	case "list_worktrees":
		return BrowserEvent{Type: EventListWorktrees}
	case "select_agent":
		return BrowserEvent{Type: EventSelectAgent, ID: cmd.ID}
	case "create_agent":
		return BrowserEvent{
			Type:          EventCreateAgent,
			IssueOrBranch: cmd.IssueOrBranch,
			Prompt:        cmd.Prompt,
		}
	case "reopen_worktree":
		return BrowserEvent{
			Type:   EventReopenWorktree,
			Path:   cmd.Path,
			Branch: cmd.Branch,
			Prompt: cmd.Prompt,
		}
	case "delete_agent":
		deleteWorktree := false
		if cmd.DeleteWorktree != nil {
			deleteWorktree = *cmd.DeleteWorktree
		}
		return BrowserEvent{
			Type:           EventDeleteAgent,
			ID:             cmd.ID,
			DeleteWorktree: deleteWorktree,
		}
	case "toggle_pty_view":
		return BrowserEvent{Type: EventTogglePtyView}
	case "scroll":
		lines := uint32(10)
		if cmd.Lines != nil {
			lines = *cmd.Lines
		}
		return BrowserEvent{
			Type:      EventScroll,
			Direction: cmd.Direction,
			Lines:     lines,
		}
	case "scroll_to_bottom":
		return BrowserEvent{Type: EventScrollToBottom}
	case "scroll_to_top":
		return BrowserEvent{Type: EventScrollToTop}
	case "resize":
		return BrowserEvent{
			Type:   EventResize,
			Resize: &BrowserResize{Cols: cmd.Cols, Rows: cmd.Rows},
		}
	default:
		return BrowserEvent{Type: EventDisconnected}
	}
}
