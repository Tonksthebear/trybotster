// Package agent provides PTY session management for botster-hub agents.
//
// Each agent runs in a git worktree with dedicated PTY sessions for the
// CLI process and optionally a dev server. The agent is process-agnostic -
// it runs whatever the user configures via .botster_init scripts.
package agent

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"

	"github.com/creack/pty"
	"github.com/google/uuid"

	"github.com/trybotster/botster-hub/internal/notification"
	"github.com/trybotster/botster-hub/internal/vt100"
)

// Status represents the current state of an agent.
type Status string

const (
	StatusInitializing Status = "initializing"
	StatusRunning      Status = "running"
	StatusCompleted    Status = "completed"
	StatusFailed       Status = "failed"
)

// Agent represents a running agent in a git worktree.
type Agent struct {
	// ID is the unique identifier for this agent.
	ID uuid.UUID

	// Repo is the repository name in "owner/repo" format.
	Repo string

	// IssueNumber is the GitHub issue number (if applicable).
	IssueNumber *int

	// BranchName is the git branch name.
	BranchName string

	// WorktreePath is the path to the git worktree.
	WorktreePath string

	// StartTime is when the agent was created.
	StartTime time.Time

	// LastActivity is when output was last received.
	LastActivity time.Time

	// Status is the current execution status.
	Status Status

	// TunnelPort is the port for HTTP tunnel forwarding.
	TunnelPort *int

	// cliPTY is the primary PTY session.
	cliPTY *PTYSession

	// serverPTY is the optional dev server PTY.
	serverPTY *PTYSession

	// activePTY tracks which PTY is currently displayed.
	activePTY PTYView

	// scrollOffset tracks scroll position per PTY view.
	cliScrollOffset    int
	serverScrollOffset int

	// notificationChan receives detected notifications.
	notificationChan chan notification.Notification

	mu sync.RWMutex
}

// PTYView indicates which PTY is active.
type PTYView int

const (
	PTYViewCLI PTYView = iota
	PTYViewServer
)

// PTYSession manages a pseudo-terminal session.
type PTYSession struct {
	// pty is the master side of the pseudo-terminal.
	pty *os.File

	// cmd is the running command.
	cmd *exec.Cmd

	// parser is the VT100 terminal emulator for screen state.
	parser *vt100.Parser

	// buffer holds line-based output for pattern detection.
	buffer *RingBuffer

	// rawOutput holds raw bytes for browser streaming.
	rawOutput *RingBuffer

	// lastScreenHash tracks screen changes.
	lastScreenHash uint64

	// rows and cols are the terminal dimensions.
	rows, cols uint16

	mu sync.RWMutex
}

// RingBuffer is a fixed-size buffer that drops old data.
type RingBuffer struct {
	data [][]byte
	max  int
	mu   sync.Mutex
}

// NewRingBuffer creates a new ring buffer with the given capacity.
func NewRingBuffer(capacity int) *RingBuffer {
	return &RingBuffer{
		data: make([][]byte, 0, capacity),
		max:  capacity,
	}
}

// Push adds data to the buffer, dropping oldest if full.
func (rb *RingBuffer) Push(data []byte) {
	rb.mu.Lock()
	defer rb.mu.Unlock()

	// Make a copy of the data
	copied := make([]byte, len(data))
	copy(copied, data)

	if len(rb.data) >= rb.max {
		rb.data = rb.data[1:]
	}
	rb.data = append(rb.data, copied)
}

// Drain returns all data and clears the buffer.
func (rb *RingBuffer) Drain() []byte {
	rb.mu.Lock()
	defer rb.mu.Unlock()

	var result []byte
	for _, chunk := range rb.data {
		result = append(result, chunk...)
	}
	rb.data = rb.data[:0]
	return result
}

// New creates a new agent for the specified repository and worktree.
func New(repo string, issueNumber *int, branchName, worktreePath string) *Agent {
	now := time.Now()
	return &Agent{
		ID:               uuid.New(),
		Repo:             repo,
		IssueNumber:      issueNumber,
		BranchName:       branchName,
		WorktreePath:     worktreePath,
		StartTime:        now,
		LastActivity:     now,
		Status:           StatusInitializing,
		activePTY:        PTYViewCLI,
		notificationChan: make(chan notification.Notification, 100),
	}
}

// Spawn starts the CLI PTY with the given command and specified dimensions.
// The command is sourced in an interactive bash shell so bash stays open after
// the command completes, allowing the user to continue working.
func (a *Agent) Spawn(command string, env map[string]string, rows, cols uint16) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Use provided dimensions, fallback to reasonable defaults
	if rows == 0 {
		rows = 24
	}
	if cols == 0 {
		cols = 80
	}

	// Create PTY session with correct dimensions
	session, err := newPTYSession(rows, cols)
	if err != nil {
		return fmt.Errorf("failed to create PTY session: %w", err)
	}

	// Start an interactive bash shell
	// We'll source the init script after bash starts
	cmd := exec.Command("bash", "-i")
	cmd.Dir = a.WorktreePath

	// Set environment - start with current env
	cmd.Env = os.Environ()

	// Set TERM for proper terminal emulation
	cmd.Env = append(cmd.Env, "TERM=xterm-256color")

	// Add user-provided environment variables
	for k, v := range env {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%s", k, v))
	}

	// Start with PTY
	ptmx, err := pty.Start(cmd)
	if err != nil {
		return fmt.Errorf("failed to start PTY: %w", err)
	}

	// Set PTY size immediately
	if err := pty.Setsize(ptmx, &pty.Winsize{Rows: rows, Cols: cols}); err != nil {
		// Non-fatal, continue
	}

	session.pty = ptmx
	session.cmd = cmd
	a.cliPTY = session
	a.Status = StatusRunning

	// Start reader goroutine
	go a.readPTY(session)

	// Source the init script if provided (non-blocking write to PTY)
	if command != "" {
		// Give bash a moment to initialize
		go func() {
			time.Sleep(100 * time.Millisecond)
			// Send the command to the PTY
			ptmx.Write([]byte(command + "\n"))
		}()
	}

	return nil
}

// newPTYSession creates a new PTY session with the given dimensions.
func newPTYSession(rows, cols uint16) (*PTYSession, error) {
	return &PTYSession{
		rows:      rows,
		cols:      cols,
		parser:    vt100.New(int(rows), int(cols)),
		buffer:    NewRingBuffer(20000), // 20K lines
		rawOutput: NewRingBuffer(1000),  // 1000 chunks for streaming
	}, nil
}

// readPTY reads from the PTY and buffers output.
func (a *Agent) readPTY(session *PTYSession) {
	buf := make([]byte, 4096)
	for {
		n, err := session.pty.Read(buf)
		if err != nil {
			if err != io.EOF {
				// Log error but don't crash
			}
			return
		}

		if n > 0 {
			data := buf[:n]

			// Update last activity time
			a.mu.Lock()
			a.LastActivity = time.Now()
			a.mu.Unlock()

			// Buffer raw output for streaming
			session.rawOutput.Push(data)
			session.buffer.Push(data)

			// Feed to VT100 parser for screen state
			session.mu.Lock()
			if session.parser != nil {
				session.parser.Process(data)
			}
			session.mu.Unlock()

			// Detect terminal notifications
			notifications := notification.Detect(data)
			for _, n := range notifications {
				select {
				case a.notificationChan <- n:
				default:
					// Channel full, drop notification
				}
			}
		}
	}
}

// WriteInput sends input to the active PTY.
func (a *Agent) WriteInput(input []byte) error {
	a.mu.RLock()
	defer a.mu.RUnlock()

	session := a.getActivePTY()
	if session == nil || session.pty == nil {
		return fmt.Errorf("no active PTY")
	}

	_, err := session.pty.Write(input)
	return err
}

// getActivePTY returns the currently active PTY session.
func (a *Agent) getActivePTY() *PTYSession {
	switch a.activePTY {
	case PTYViewServer:
		if a.serverPTY != nil {
			return a.serverPTY
		}
		return a.cliPTY
	default:
		return a.cliPTY
	}
}

// DrainRawOutput returns accumulated raw PTY output.
func (a *Agent) DrainRawOutput() []byte {
	a.mu.RLock()
	defer a.mu.RUnlock()

	session := a.getActivePTY()
	if session == nil {
		return nil
	}

	return session.rawOutput.Drain()
}

// Resize changes the PTY dimensions.
func (a *Agent) Resize(rows, cols uint16) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.cliPTY != nil && a.cliPTY.pty != nil {
		if err := pty.Setsize(a.cliPTY.pty, &pty.Winsize{
			Rows: rows,
			Cols: cols,
		}); err != nil {
			return err
		}
		a.cliPTY.rows = rows
		a.cliPTY.cols = cols
		// Resize VT100 parser
		a.cliPTY.mu.Lock()
		if a.cliPTY.parser != nil {
			a.cliPTY.parser.SetSize(int(rows), int(cols))
		}
		a.cliPTY.mu.Unlock()
	}

	if a.serverPTY != nil && a.serverPTY.pty != nil {
		if err := pty.Setsize(a.serverPTY.pty, &pty.Winsize{
			Rows: rows,
			Cols: cols,
		}); err != nil {
			return err
		}
		a.serverPTY.rows = rows
		a.serverPTY.cols = cols
		// Resize VT100 parser
		a.serverPTY.mu.Lock()
		if a.serverPTY.parser != nil {
			a.serverPTY.parser.SetSize(int(rows), int(cols))
		}
		a.serverPTY.mu.Unlock()
	}

	return nil
}

// TogglePTYView switches between CLI and Server PTY views.
func (a *Agent) TogglePTYView() {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.activePTY == PTYViewCLI && a.serverPTY != nil {
		a.activePTY = PTYViewServer
	} else {
		a.activePTY = PTYViewCLI
	}
}

// SessionKey returns a unique key for this agent session.
// Format: "owner-repo-42" for issues, "owner-repo-branch-name" for branches.
func (a *Agent) SessionKey() string {
	// Sanitize repo name: replace / with -
	repoSafe := strings.ReplaceAll(a.Repo, "/", "-")
	if a.IssueNumber != nil {
		return fmt.Sprintf("%s-%d", repoSafe, *a.IssueNumber)
	}
	// Sanitize branch name: replace / with -
	branchSafe := strings.ReplaceAll(a.BranchName, "/", "-")
	return fmt.Sprintf("%s-%s", repoSafe, branchSafe)
}

// Age returns how long the agent has been running.
func (a *Agent) Age() time.Duration {
	return time.Since(a.StartTime)
}

// Close terminates the agent and cleans up resources.
func (a *Agent) Close() error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.cliPTY != nil && a.cliPTY.cmd != nil {
		a.cliPTY.cmd.Process.Kill()
		a.cliPTY.cmd.Wait()
	}

	if a.serverPTY != nil && a.serverPTY.cmd != nil {
		a.serverPTY.cmd.Process.Kill()
		a.serverPTY.cmd.Wait()
	}

	return nil
}

// --- AgentSession interface methods (for SSH server) ---

// GetID returns the agent's unique identifier as a string.
func (a *Agent) GetID() string {
	return a.ID.String()
}

// Read reads from the active PTY output (for SSH streaming).
// This directly reads from the PTY, blocking until data is available.
func (a *Agent) Read(p []byte) (n int, err error) {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil || session.pty == nil {
		return 0, fmt.Errorf("no active PTY")
	}

	return session.pty.Read(p)
}

// Write writes to the active PTY input (for SSH streaming).
func (a *Agent) Write(p []byte) (n int, err error) {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil || session.pty == nil {
		return 0, fmt.Errorf("no active PTY")
	}

	return session.pty.Write(p)
}

// ResizeSSH resizes the PTY for SSH sessions.
func (a *Agent) ResizeSSH(rows, cols int) error {
	return a.Resize(uint16(rows), uint16(cols))
}

// --- Screen and VT100 methods ---

// GetScreen returns the visible screen content from the active PTY.
func (a *Agent) GetScreen() []string {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return nil
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return nil
	}

	return session.parser.GetScreen()
}

// GetScreenAsANSI returns the screen with ANSI escape sequences.
func (a *Agent) GetScreenAsANSI() string {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return ""
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return ""
	}

	return session.parser.GetScreenAsANSI()
}

// GetScreenForTUI returns screen lines with SGR styling codes only.
// Safe to embed in a TUI panel - no cursor movement or screen control sequences.
func (a *Agent) GetScreenForTUI() []string {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return nil
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return nil
	}

	return session.parser.GetScreenForTUI()
}

// GetScreenCells returns the raw cell content and format for direct TUI rendering.
// This enables true cell-by-cell rendering like ratatui.
func (a *Agent) GetScreenCells() [][]vt100.CellInfo {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return nil
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return nil
	}

	return session.parser.GetScreenCells()
}

// GetScreenHash returns a hash of the current screen content.
func (a *Agent) GetScreenHash() uint64 {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return 0
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return 0
	}

	return session.parser.GetScreenHash()
}

// HasScreenChanged returns true if the screen changed since last check.
func (a *Agent) HasScreenChanged() bool {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return false
	}

	session.mu.Lock()
	defer session.mu.Unlock()

	if session.parser == nil {
		return false
	}

	hash := session.parser.GetScreenHash()
	changed := hash != session.lastScreenHash
	session.lastScreenHash = hash
	return changed
}

// --- Server PTY methods ---

// SpawnServer starts the server PTY with the given command and dimensions.
// Like the CLI PTY, this uses an interactive bash shell so when the server
// process exits, the user is dropped back into a bash prompt.
func (a *Agent) SpawnServer(command string, env map[string]string, rows, cols uint16) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Use provided dimensions, fallback to reasonable defaults
	if rows == 0 {
		rows = 24
	}
	if cols == 0 {
		cols = 80
	}

	// Create PTY session with correct dimensions
	session, err := newPTYSession(rows, cols)
	if err != nil {
		return fmt.Errorf("failed to create server PTY session: %w", err)
	}

	// Start an interactive bash shell (stays alive after command exits)
	cmd := exec.Command("bash", "-i")
	cmd.Dir = a.WorktreePath

	// Set environment with TERM
	cmd.Env = os.Environ()
	cmd.Env = append(cmd.Env, "TERM=xterm-256color")
	for k, v := range env {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%s", k, v))
	}

	// Start with PTY
	ptmx, err := pty.Start(cmd)
	if err != nil {
		return fmt.Errorf("failed to start server PTY: %w", err)
	}

	// Set PTY size immediately
	if err := pty.Setsize(ptmx, &pty.Winsize{Rows: rows, Cols: cols}); err != nil {
		// Non-fatal, continue
	}

	session.pty = ptmx
	session.cmd = cmd
	a.serverPTY = session

	// Start reader goroutine
	go a.readPTY(session)

	// Send the command to the PTY after bash initializes
	if command != "" {
		go func() {
			time.Sleep(100 * time.Millisecond)
			ptmx.Write([]byte(command + "\n"))
		}()
	}

	return nil
}

// HasServerPTY returns true if a server PTY is running.
func (a *Agent) HasServerPTY() bool {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.serverPTY != nil && a.serverPTY.cmd != nil
}

// GetActivePTYView returns which PTY view is currently active.
func (a *Agent) GetActivePTYView() PTYView {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.activePTY
}

// --- Scroll methods ---

// ScrollUp scrolls the active PTY view up by the given number of lines.
func (a *Agent) ScrollUp(lines int) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.activePTY == PTYViewServer {
		a.serverScrollOffset += lines
		// Cap at scrollback size
		if a.serverPTY != nil && a.serverPTY.parser != nil {
			max := a.serverPTY.parser.ScrollbackCount()
			if a.serverScrollOffset > max {
				a.serverScrollOffset = max
			}
		}
	} else {
		a.cliScrollOffset += lines
		// Cap at scrollback size
		if a.cliPTY != nil && a.cliPTY.parser != nil {
			max := a.cliPTY.parser.ScrollbackCount()
			if a.cliScrollOffset > max {
				a.cliScrollOffset = max
			}
		}
	}
}

// ScrollDown scrolls the active PTY view down by the given number of lines.
func (a *Agent) ScrollDown(lines int) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.activePTY == PTYViewServer {
		a.serverScrollOffset -= lines
		if a.serverScrollOffset < 0 {
			a.serverScrollOffset = 0
		}
	} else {
		a.cliScrollOffset -= lines
		if a.cliScrollOffset < 0 {
			a.cliScrollOffset = 0
		}
	}
}

// ScrollReset resets the scroll offset to show the latest content.
func (a *Agent) ScrollReset() {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.activePTY == PTYViewServer {
		a.serverScrollOffset = 0
	} else {
		a.cliScrollOffset = 0
	}
}

// ScrollToTop scrolls to the oldest content in the scrollback buffer.
func (a *Agent) ScrollToTop() {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.activePTY == PTYViewServer {
		if a.serverPTY != nil && a.serverPTY.parser != nil {
			a.serverScrollOffset = a.serverPTY.parser.ScrollbackCount()
		}
	} else {
		if a.cliPTY != nil && a.cliPTY.parser != nil {
			a.cliScrollOffset = a.cliPTY.parser.ScrollbackCount()
		}
	}
}

// ScrollToBottom scrolls to show the latest content (alias for ScrollReset).
func (a *Agent) ScrollToBottom() {
	a.ScrollReset()
}

// GetScrollOffset returns the current scroll offset for the active PTY.
func (a *Agent) GetScrollOffset() int {
	a.mu.RLock()
	defer a.mu.RUnlock()

	if a.activePTY == PTYViewServer {
		return a.serverScrollOffset
	}
	return a.cliScrollOffset
}

// --- Notification methods ---

// Notifications returns the channel for receiving terminal notifications.
func (a *Agent) Notifications() <-chan notification.Notification {
	return a.notificationChan
}

// --- Activity methods ---

// GetLastActivity returns when output was last received.
func (a *Agent) GetLastActivity() time.Time {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.LastActivity
}

// TimeSinceLastActivity returns the duration since last output.
func (a *Agent) TimeSinceLastActivity() time.Duration {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return time.Since(a.LastActivity)
}

// --- Scrollback methods ---

// GetScrollback returns the scrollback buffer from the active PTY.
func (a *Agent) GetScrollback() []string {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return nil
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return nil
	}

	return session.parser.GetScrollback()
}

// ScrollbackCount returns the number of lines in the scrollback buffer.
func (a *Agent) ScrollbackCount() int {
	a.mu.RLock()
	session := a.getActivePTY()
	a.mu.RUnlock()

	if session == nil {
		return 0
	}

	session.mu.RLock()
	defer session.mu.RUnlock()

	if session.parser == nil {
		return 0
	}

	return session.parser.ScrollbackCount()
}
