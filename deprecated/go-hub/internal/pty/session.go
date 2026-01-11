// Package pty provides pseudo-terminal session management for agents.
//
// Each agent can have multiple PTY sessions (CLI and server) running concurrently.
// This package handles PTY creation, I/O, resizing, and cleanup.
package pty

import (
	"io"
	"log/slog"
	"os"
	"os/exec"
	"sync"

	"github.com/creack/pty"
)

// MaxBufferLines is the maximum lines to keep in scrollback buffer.
// 20K lines balances memory usage (~2-4MB per agent) with sufficient
// history for debugging.
const MaxBufferLines = 20000

// Notification represents a terminal notification (OSC 9/777).
// Will be implemented in notification package.
type Notification struct {
	Type    string // "osc9" or "osc777"
	Title   string
	Message string
}

// Session encapsulates all state for a single PTY session.
//
// Each PTY session manages:
// - A pseudo-terminal for process I/O
// - A line buffer for pattern detection
// - Raw output queue for browser streaming
// - Notification detection
type Session struct {
	// ptyFile is the master PTY file descriptor.
	ptyFile *os.File

	// cmd is the running command.
	cmd *exec.Cmd

	// rows and cols are the current terminal dimensions.
	rows uint16
	cols uint16

	// buffer is line-based history for pattern detection.
	buffer     []string
	bufferLock sync.Mutex

	// rawOutput is queued raw bytes for browser streaming.
	rawOutput     [][]byte
	rawOutputLock sync.Mutex

	// notificationChan receives detected OSC notifications.
	notificationChan chan Notification

	// done signals reader goroutine to stop.
	done chan struct{}

	// readerWg waits for reader goroutine to finish.
	readerWg sync.WaitGroup

	// logger for this session.
	logger *slog.Logger
}

// New creates a new PTY session with the specified dimensions.
func New(rows, cols uint16, logger *slog.Logger) *Session {
	if logger == nil {
		logger = slog.Default()
	}
	return &Session{
		rows:             rows,
		cols:             cols,
		buffer:           make([]string, 0),
		rawOutput:        make([][]byte, 0),
		notificationChan: make(chan Notification, 10),
		done:             make(chan struct{}),
		logger:           logger,
	}
}

// SpawnConfig holds configuration for spawning a process in the PTY.
type SpawnConfig struct {
	// Command is the command to run (e.g., "bash", "echo hello").
	Command string

	// Args are additional arguments.
	Args []string

	// Dir is the working directory.
	Dir string

	// Env are environment variables (key=value format).
	Env []string

	// InitCommands are commands to send after spawn.
	InitCommands []string
}

// Spawn starts a process in the PTY.
//
// This function:
// 1. Creates a PTY with the current dimensions
// 2. Spawns the command in the PTY
// 3. Starts the reader goroutine
//
// Returns an error if PTY creation or command spawn fails.
func (s *Session) Spawn(cfg SpawnConfig) error {
	// Build command
	args := cfg.Args
	if len(args) == 0 && cfg.Command != "" {
		// Parse command string for simple cases
		args = []string{"-c", cfg.Command}
		cfg.Command = "/bin/bash"
	}

	cmd := exec.Command(cfg.Command, args...)
	cmd.Dir = cfg.Dir
	cmd.Env = append(os.Environ(), cfg.Env...)

	// Start PTY
	ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{
		Rows: s.rows,
		Cols: s.cols,
	})
	if err != nil {
		return err
	}

	s.ptyFile = ptmx
	s.cmd = cmd

	// Start reader goroutine
	s.readerWg.Add(1)
	go s.readerLoop()

	s.logger.Info("PTY spawned", "command", cfg.Command, "dir", cfg.Dir)

	// Send init commands
	for _, initCmd := range cfg.InitCommands {
		s.WriteString(initCmd + "\n")
	}

	return nil
}

// readerLoop reads from PTY and processes output.
func (s *Session) readerLoop() {
	defer s.readerWg.Done()

	buf := make([]byte, 4096)
	currentLine := ""

	for {
		select {
		case <-s.done:
			return
		default:
		}

		n, err := s.ptyFile.Read(buf)
		if err != nil {
			if err != io.EOF {
				s.logger.Error("PTY read error", "error", err)
			}
			return
		}

		if n == 0 {
			continue
		}

		chunk := buf[:n]

		// Queue raw output for browser streaming
		s.rawOutputLock.Lock()
		s.rawOutput = append(s.rawOutput, append([]byte{}, chunk...))
		s.rawOutputLock.Unlock()

		// Process lines for buffer - convert to string first to handle UTF-8 properly
		// (Iterating over []byte can split multi-byte UTF-8 sequences like CJK chars)
		chunkStr := string(chunk)
		for _, r := range chunkStr {
			if r == '\n' {
				s.addToBuffer(currentLine)
				currentLine = ""
			} else if r != '\r' {
				currentLine += string(r)
			}
		}

		// TODO: Detect OSC notifications here
		// This will be implemented in the notification package
	}
}

// addToBuffer adds a line to the buffer, respecting MaxBufferLines.
func (s *Session) addToBuffer(line string) {
	s.bufferLock.Lock()
	defer s.bufferLock.Unlock()

	s.buffer = append(s.buffer, line)
	if len(s.buffer) > MaxBufferLines {
		s.buffer = s.buffer[1:]
	}
}

// Write writes input bytes to the PTY.
func (s *Session) Write(p []byte) (n int, err error) {
	if s.ptyFile == nil {
		return 0, nil
	}
	return s.ptyFile.Write(p)
}

// WriteString writes a string to the PTY.
func (s *Session) WriteString(str string) (n int, err error) {
	return s.Write([]byte(str))
}

// Read reads from the PTY output.
// Implements io.Reader for SSH integration.
func (s *Session) Read(p []byte) (n int, err error) {
	if s.ptyFile == nil {
		return 0, io.EOF
	}
	return s.ptyFile.Read(p)
}

// Resize changes the PTY dimensions.
func (s *Session) Resize(rows, cols uint16) error {
	s.rows = rows
	s.cols = cols

	if s.ptyFile != nil {
		return pty.Setsize(s.ptyFile, &pty.Winsize{
			Rows: rows,
			Cols: cols,
		})
	}
	return nil
}

// ResizeSSH resizes the PTY for SSH sessions.
// Implements AgentSession interface.
func (s *Session) ResizeSSH(rows, cols int) error {
	return s.Resize(uint16(rows), uint16(cols))
}

// GetID returns a placeholder ID. Real ID comes from Agent.
func (s *Session) GetID() string {
	return ""
}

// Kill terminates the child process.
func (s *Session) Kill() error {
	// Signal reader to stop
	close(s.done)

	// Kill the process
	if s.cmd != nil && s.cmd.Process != nil {
		s.logger.Info("Killing PTY child process")
		if err := s.cmd.Process.Kill(); err != nil {
			s.logger.Warn("Failed to kill PTY child", "error", err)
		}
		// Wait to prevent zombies
		s.cmd.Wait()
	}

	// Close PTY file
	if s.ptyFile != nil {
		s.ptyFile.Close()
	}

	// Wait for reader to finish
	s.readerWg.Wait()

	return nil
}

// IsSpawned returns true if a process is running.
func (s *Session) IsSpawned() bool {
	return s.ptyFile != nil
}

// DrainRawOutput returns and clears all queued raw output.
// Used for browser streaming.
func (s *Session) DrainRawOutput() []byte {
	s.rawOutputLock.Lock()
	defer s.rawOutputLock.Unlock()

	var result []byte
	for _, chunk := range s.rawOutput {
		result = append(result, chunk...)
	}
	s.rawOutput = s.rawOutput[:0]
	return result
}

// GetBufferSnapshot returns a copy of the line buffer.
func (s *Session) GetBufferSnapshot() []string {
	s.bufferLock.Lock()
	defer s.bufferLock.Unlock()

	result := make([]string, len(s.buffer))
	copy(result, s.buffer)
	return result
}

// NotificationChan returns the channel for receiving notifications.
func (s *Session) NotificationChan() <-chan Notification {
	return s.notificationChan
}

// Size returns current dimensions.
func (s *Session) Size() (rows, cols uint16) {
	return s.rows, s.cols
}
