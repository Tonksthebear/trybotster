package relay

import (
	"encoding/json"
	"strconv"
	"strings"
	"sync"
)

// TerminalOutputSender handles sending terminal output to the browser.
type TerminalOutputSender struct {
	ch     chan<- string
	closed bool
	mu     sync.RWMutex
}

// NewTerminalOutputSender creates a new terminal output sender.
func NewTerminalOutputSender(ch chan<- string) *TerminalOutputSender {
	return &TerminalOutputSender{ch: ch}
}

// Send sends terminal output to browser.
func (s *TerminalOutputSender) Send(output string) error {
	s.mu.RLock()
	defer s.mu.RUnlock()

	if s.closed {
		return nil
	}

	select {
	case s.ch <- output:
		return nil
	default:
		// Channel full, drop message
		return nil
	}
}

// IsClosed checks if the channel is closed.
func (s *TerminalOutputSender) IsClosed() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.closed
}

// Close marks the sender as closed.
func (s *TerminalOutputSender) Close() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.closed = true
}

// BrowserState consolidates all browser-related state.
type BrowserState struct {
	// Terminal output sender for Tailscale SSH relay.
	Sender *TerminalOutputSender
	// Browser event receiver channel.
	EventCh <-chan BrowserEvent
	// Whether a browser is currently connected.
	Connected bool
	// Browser terminal dimensions.
	Dims *BrowserResize
	// Browser display mode (TUI or GUI).
	Mode *BrowserMode
	// Last screen hash per agent (bandwidth optimization).
	AgentScreenHashes map[string]uint64
	// Last screen hash sent to browser.
	LastScreenHash *uint64
	// Tailscale connection URL for QR code generation.
	TailscaleConnectionURL string
	// CLI's tailnet hostname for browser to connect to.
	TailscaleHostname string

	mu sync.RWMutex
}

// NewBrowserState creates a new browser state.
func NewBrowserState() *BrowserState {
	return &BrowserState{
		AgentScreenHashes: make(map[string]uint64),
	}
}

// IsConnected checks if browser is connected and ready.
func (s *BrowserState) IsConnected() bool {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.Connected && s.Sender != nil
}

// SetConnected sets connection established with sender and receiver.
func (s *BrowserState) SetConnected(sender *TerminalOutputSender, eventCh <-chan BrowserEvent) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Sender = sender
	s.EventCh = eventCh
	s.Connected = false // Will be true after Connected event
}

// HandleConnected handles browser connected event.
func (s *BrowserState) HandleConnected(deviceName string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Connected = true
	gui := BrowserModeGUI
	s.Mode = &gui
}

// HandleDisconnected handles browser disconnected event.
func (s *BrowserState) HandleDisconnected() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Connected = false
	s.Dims = nil
	s.LastScreenHash = nil
}

// HandleResize handles browser resize event.
func (s *BrowserState) HandleResize(resize BrowserResize) (uint16, uint16) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.Dims = &resize
	s.LastScreenHash = nil
	return resize.Rows, resize.Cols
}

// HandleSetMode handles browser mode change.
func (s *BrowserState) HandleSetMode(mode string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if mode == "gui" {
		gui := BrowserModeGUI
		s.Mode = &gui
	} else {
		tui := BrowserModeTUI
		s.Mode = &tui
	}
	s.LastScreenHash = nil
}

// Disconnect handles browser disconnection.
func (s *BrowserState) Disconnect() {
	s.HandleDisconnected()
}

// InvalidateScreen forces re-send of screen.
func (s *BrowserState) InvalidateScreen() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.LastScreenHash = nil
}

// DrainEvents drains pending events from receiver.
func (s *BrowserState) DrainEvents() []BrowserEvent {
	s.mu.RLock()
	ch := s.EventCh
	s.mu.RUnlock()

	if ch == nil {
		return nil
	}

	var events []BrowserEvent
	for {
		select {
		case event, ok := <-ch:
			if !ok {
				return events
			}
			events = append(events, event)
		default:
			return events
		}
	}
}

// SetTailscaleInfo sets Tailscale connection info for QR code generation.
func (s *BrowserState) SetTailscaleInfo(connectionURL, hostname string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.TailscaleConnectionURL = connectionURL
	s.TailscaleHostname = hostname
}

// GetDimsWithMode returns current browser dimensions with mode.
func (s *BrowserState) GetDimsWithMode() *BrowserDimsWithMode {
	s.mu.RLock()
	defer s.mu.RUnlock()

	if s.Dims == nil || s.Mode == nil {
		return nil
	}
	return &BrowserDimsWithMode{
		Rows: s.Dims.Rows,
		Cols: s.Dims.Cols,
		Mode: *s.Mode,
	}
}

// GetMode returns the current browser mode.
func (s *BrowserState) GetMode() *BrowserMode {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.Mode
}

// BuildWorktreeInfo creates a WorktreeInfo from path and branch.
func BuildWorktreeInfo(path, branch string) WorktreeInfo {
	var issueNumber *uint64
	if after, found := strings.CutPrefix(branch, "botster-issue-"); found {
		if num, err := strconv.ParseUint(after, 10, 64); err == nil {
			issueNumber = &num
		}
	}
	return WorktreeInfo{
		Path:        path,
		Branch:      branch,
		IssueNumber: issueNumber,
	}
}

// SendAgentList sends agent list to connected browser.
func SendAgentList(sender *TerminalOutputSender, agents []AgentInfo) error {
	msg := AgentsMessage(agents)
	return sendJSON(sender, msg)
}

// SendWorktreeList sends worktree list to connected browser.
func SendWorktreeList(sender *TerminalOutputSender, worktrees []WorktreeInfo, repo string) error {
	msg := WorktreesMessage(worktrees, repo)
	return sendJSON(sender, msg)
}

// SendAgentSelected sends agent selection notification to browser.
func SendAgentSelected(sender *TerminalOutputSender, agentID string) error {
	msg := AgentSelectedMessage(agentID)
	return sendJSON(sender, msg)
}

// SendOutput sends terminal output to browser.
func SendOutput(sender *TerminalOutputSender, output string) error {
	return sender.Send(output)
}

// SendScrollback sends scrollback history to browser.
func SendScrollback(sender *TerminalOutputSender, lines []string) error {
	msg := ScrollbackMessage(lines)
	return sendJSON(sender, msg)
}

// SendError sends error message to browser.
func SendError(sender *TerminalOutputSender, errMsg string) error {
	msg := ErrorMessage(errMsg)
	return sendJSON(sender, msg)
}

func sendJSON(sender *TerminalOutputSender, msg TerminalMessage) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	return sender.Send(string(data))
}

// CalculateAgentDims calculates agent dimensions based on browser mode.
func CalculateAgentDims(dims *BrowserResize, mode BrowserMode) (uint16, uint16) {
	if mode == BrowserModeGUI {
		return dims.Cols, dims.Rows
	}
	// TUI mode - use 70% width
	tuiCols := (dims.Cols * 70 / 100) - 2
	tuiRows := dims.Rows - 2
	return tuiCols, tuiRows
}

// GetOutputForMode returns output based on browser mode.
func GetOutputForMode(mode *BrowserMode, tuiOutput string, agentOutput *string) string {
	if mode != nil && *mode == BrowserModeGUI {
		if agentOutput != nil {
			return *agentOutput
		}
		return "\x1b[2J\x1b[HNo agent selected"
	}
	return tuiOutput
}
