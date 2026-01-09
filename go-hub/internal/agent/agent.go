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
	"sync"
	"time"

	"github.com/creack/pty"
	"github.com/google/uuid"
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

	// buffer holds line-based output for pattern detection.
	buffer *RingBuffer

	// rawOutput holds raw bytes for browser streaming.
	rawOutput *RingBuffer

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
	return &Agent{
		ID:           uuid.New(),
		Repo:         repo,
		IssueNumber:  issueNumber,
		BranchName:   branchName,
		WorktreePath: worktreePath,
		StartTime:    time.Now(),
		Status:       StatusInitializing,
		activePTY:    PTYViewCLI,
	}
}

// Spawn starts the CLI PTY with the given command.
func (a *Agent) Spawn(command string, env map[string]string) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Create PTY session
	session, err := newPTYSession(24, 80)
	if err != nil {
		return fmt.Errorf("failed to create PTY session: %w", err)
	}

	// Build command
	cmd := exec.Command("bash", "-c", command)
	cmd.Dir = a.WorktreePath

	// Set environment
	cmd.Env = os.Environ()
	for k, v := range env {
		cmd.Env = append(cmd.Env, fmt.Sprintf("%s=%s", k, v))
	}

	// Start with PTY
	ptmx, err := pty.Start(cmd)
	if err != nil {
		return fmt.Errorf("failed to start PTY: %w", err)
	}

	session.pty = ptmx
	session.cmd = cmd
	a.cliPTY = session
	a.Status = StatusRunning

	// Start reader goroutine
	go a.readPTY(session)

	return nil
}

// newPTYSession creates a new PTY session with the given dimensions.
func newPTYSession(rows, cols uint16) (*PTYSession, error) {
	return &PTYSession{
		rows:      rows,
		cols:      cols,
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
			session.rawOutput.Push(data)
			session.buffer.Push(data)
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
func (a *Agent) SessionKey() string {
	repoSafe := a.Repo // TODO: sanitize
	if a.IssueNumber != nil {
		return fmt.Sprintf("%s-%d", repoSafe, *a.IssueNumber)
	}
	return fmt.Sprintf("%s-%s", repoSafe, a.BranchName)
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
