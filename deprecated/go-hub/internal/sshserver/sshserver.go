// Package sshserver provides an SSH server over tsnet for browser terminal access.
//
// The browser connects via SSH to view and interact with agent PTY sessions.
// This runs entirely over the Tailscale mesh network (no port exposure).
package sshserver

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"sync"

	"github.com/gliderlabs/ssh"
)

// AgentSession represents a PTY session that can be attached to.
type AgentSession interface {
	// GetID returns the agent's unique identifier.
	GetID() string

	// Read reads from the PTY output.
	Read(p []byte) (n int, err error)

	// Write writes to the PTY input.
	Write(p []byte) (n int, err error)

	// ResizeSSH resizes the PTY for SSH sessions.
	ResizeSSH(rows, cols int) error
}

// SessionProvider provides access to agent sessions.
type SessionProvider interface {
	// GetSession returns an agent session by ID.
	GetSession(agentID string) (AgentSession, bool)

	// ListSessions returns all active session IDs.
	ListSessions() []string
}

// Server is an SSH server for browser terminal access.
type Server struct {
	listener net.Listener
	provider SessionProvider
	logger   *slog.Logger

	mu       sync.Mutex
	sessions map[string]*browserSession // SSH session ID -> browser session
}

type browserSession struct {
	agentID string
	ssh     ssh.Session
}

// New creates a new SSH server.
func New(listener net.Listener, provider SessionProvider, logger *slog.Logger) *Server {
	return &Server{
		listener: listener,
		provider: provider,
		logger:   logger,
		sessions: make(map[string]*browserSession),
	}
}

// Serve starts the SSH server.
func (s *Server) Serve(ctx context.Context) error {
	server := &ssh.Server{
		Handler: s.handleSession,
		PtyCallback: func(ctx ssh.Context, pty ssh.Pty) bool {
			return true // Allow PTY allocation
		},
		SubsystemHandlers: map[string]ssh.SubsystemHandler{
			"sftp": nil, // Disable SFTP
		},
	}

	// Accept connections until context is cancelled
	go func() {
		<-ctx.Done()
		s.listener.Close()
	}()

	s.logger.Info("SSH server starting", "addr", s.listener.Addr())

	for {
		conn, err := s.listener.Accept()
		if err != nil {
			select {
			case <-ctx.Done():
				return ctx.Err()
			default:
				s.logger.Error("Accept error", "error", err)
				continue
			}
		}

		go func() {
			server.HandleConn(conn)
		}()
	}
}

func (s *Server) handleSession(session ssh.Session) {
	user := session.User()
	s.logger.Info("SSH session started", "user", user)
	defer s.logger.Info("SSH session ended", "user", user)

	// User format: "agent-<agentID>" or just view all
	agentID := ""
	if len(user) > 6 && user[:6] == "agent-" {
		agentID = user[6:]
	}

	// Get the agent session
	var agent AgentSession
	var found bool

	if agentID != "" {
		agent, found = s.provider.GetSession(agentID)
		if !found {
			fmt.Fprintf(session, "Agent %s not found\n", agentID)
			session.Exit(1)
			return
		}
	} else {
		// List available sessions
		sessions := s.provider.ListSessions()
		if len(sessions) == 0 {
			fmt.Fprintln(session, "No active agents")
			session.Exit(0)
			return
		}
		fmt.Fprintln(session, "Available agents:")
		for _, id := range sessions {
			fmt.Fprintf(session, "  ssh agent-%s@<hostname>\n", id)
		}
		session.Exit(0)
		return
	}

	// Handle window size changes
	_, winCh, _ := session.Pty()
	go func() {
		for win := range winCh {
			if err := agent.ResizeSSH(win.Height, win.Width); err != nil {
				s.logger.Warn("Failed to resize PTY", "error", err)
			}
		}
	}()

	// Bidirectional copy
	var wg sync.WaitGroup
	wg.Add(2)

	// Copy PTY output to SSH
	go func() {
		defer wg.Done()
		io.Copy(session, agent)
	}()

	// Copy SSH input to PTY
	go func() {
		defer wg.Done()
		io.Copy(agent, session)
	}()

	wg.Wait()
}

// Close shuts down the SSH server.
func (s *Server) Close() error {
	return s.listener.Close()
}
