// Package hub provides the central state management for botster-hub.
//
// The Hub is the core orchestrator that owns all application state and
// coordinates between the TUI, server polling, and browser connectivity.
// It follows a centralized state store pattern where all state changes
// flow through the Hub.
package hub

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"time"

	"github.com/trybotster/botster-hub/internal/agent"
	"github.com/trybotster/botster-hub/internal/config"
	"github.com/trybotster/botster-hub/internal/tailnet"
)

// Hub is the central orchestrator for the botster-hub application.
type Hub struct {
	// Config holds the application configuration.
	Config *config.Config

	// HubID is the unique identifier for this hub instance.
	HubID string

	// Agents maps session keys to active agents.
	Agents map[string]*agent.Agent

	// SelectedAgent is the index of the currently selected agent.
	SelectedAgent int

	// Tailnet is the Tailscale client for browser connectivity.
	Tailnet *tailnet.Client

	// Logger for structured logging.
	Logger *slog.Logger

	// PollingEnabled indicates whether message polling is active.
	PollingEnabled bool

	// LastPoll is when we last polled for messages.
	LastPoll time.Time

	// LastHeartbeat is when we last sent a heartbeat.
	LastHeartbeat time.Time

	// TerminalDims holds the current terminal dimensions (rows, cols).
	TerminalDims struct {
		Rows uint16
		Cols uint16
	}

	// ConnectionURL is the URL for browser connection (QR code).
	ConnectionURL string

	// quit signals the hub to shut down.
	quit bool

	mu sync.RWMutex
}

// New creates a new Hub with the given configuration.
func New(cfg *config.Config, logger *slog.Logger) (*Hub, error) {
	hubID := generateHubID()

	h := &Hub{
		Config:         cfg,
		HubID:          hubID,
		Agents:         make(map[string]*agent.Agent),
		Logger:         logger,
		PollingEnabled: true,
		LastPoll:       time.Now(),
		LastHeartbeat:  time.Now(),
	}

	h.TerminalDims.Rows = 24
	h.TerminalDims.Cols = 80

	return h, nil
}

// generateHubID creates a unique hub identifier.
func generateHubID() string {
	// TODO: Generate from repo path hash for persistence across restarts
	return fmt.Sprintf("%d", time.Now().UnixNano())
}

// Setup performs initial hub setup (Tailnet connection, device registration).
func (h *Hub) Setup(ctx context.Context) error {
	h.Logger.Info("Setting up hub", "hub_id", h.HubID)

	// Connect to Tailscale if Headscale is configured
	if h.Config.HeadscaleURL != "" {
		h.Logger.Info("Connecting to Headscale", "url", h.Config.HeadscaleURL)

		// TODO: Get pre-auth key from Rails server
		authKey := "" // Will be fetched from Rails

		client, err := tailnet.New(&tailnet.Config{
			HubID:        h.HubID,
			HeadscaleURL: h.Config.HeadscaleURL,
			AuthKey:      authKey,
			Ephemeral:    true,
		}, h.Logger)

		if err != nil {
			return fmt.Errorf("failed to create Tailnet client: %w", err)
		}

		if err := client.Start(ctx); err != nil {
			return fmt.Errorf("failed to connect to Tailnet: %w", err)
		}

		h.Tailnet = client

		// Get connection URL for QR code
		ips, err := client.TailscaleIPs()
		if err == nil && len(ips) > 0 {
			h.ConnectionURL = fmt.Sprintf("ssh://user@%s", ips[0])
		}
	}

	return nil
}

// Run starts the main event loop.
func (h *Hub) Run(ctx context.Context) error {
	h.Logger.Info("Starting hub event loop")

	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()

		case <-ticker.C:
			h.tick()

		default:
			if h.quit {
				return nil
			}
			// Small sleep to prevent busy loop
			time.Sleep(10 * time.Millisecond)
		}
	}
}

// tick performs periodic maintenance tasks.
func (h *Hub) tick() {
	h.mu.Lock()
	defer h.mu.Unlock()

	// Poll for messages if interval has elapsed
	if h.PollingEnabled && time.Since(h.LastPoll) >= time.Duration(h.Config.PollInterval)*time.Second {
		h.pollMessages()
		h.LastPoll = time.Now()
	}

	// Send heartbeat every 30 seconds
	if time.Since(h.LastHeartbeat) >= 30*time.Second {
		h.sendHeartbeat()
		h.LastHeartbeat = time.Now()
	}
}

// pollMessages fetches new messages from the server.
func (h *Hub) pollMessages() {
	// TODO: Implement server polling
	h.Logger.Debug("Polling for messages")
}

// sendHeartbeat registers the hub with the server.
func (h *Hub) sendHeartbeat() {
	// TODO: Implement heartbeat
	h.Logger.Debug("Sending heartbeat")
}

// SpawnAgent creates and starts a new agent.
func (h *Hub) SpawnAgent(repo string, issueNumber *int, branchName, worktreePath, command string, env map[string]string) error {
	h.mu.Lock()
	defer h.mu.Unlock()

	ag := agent.New(repo, issueNumber, branchName, worktreePath)

	if err := ag.Spawn(command, env); err != nil {
		return fmt.Errorf("failed to spawn agent: %w", err)
	}

	h.Agents[ag.SessionKey()] = ag
	h.Logger.Info("Agent spawned",
		"session_key", ag.SessionKey(),
		"worktree", worktreePath,
	)

	return nil
}

// CloseAgent terminates an agent.
func (h *Hub) CloseAgent(sessionKey string) error {
	h.mu.Lock()
	defer h.mu.Unlock()

	ag, ok := h.Agents[sessionKey]
	if !ok {
		return fmt.Errorf("agent not found: %s", sessionKey)
	}

	if err := ag.Close(); err != nil {
		return err
	}

	delete(h.Agents, sessionKey)
	h.Logger.Info("Agent closed", "session_key", sessionKey)

	return nil
}

// GetSelectedAgent returns the currently selected agent.
func (h *Hub) GetSelectedAgent() *agent.Agent {
	h.mu.RLock()
	defer h.mu.RUnlock()

	if len(h.Agents) == 0 {
		return nil
	}

	// Convert to slice for indexing
	var agents []*agent.Agent
	for _, ag := range h.Agents {
		agents = append(agents, ag)
	}

	if h.SelectedAgent >= len(agents) {
		h.SelectedAgent = 0
	}

	return agents[h.SelectedAgent]
}

// AgentCount returns the number of active agents.
func (h *Hub) AgentCount() int {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return len(h.Agents)
}

// SetTerminalDims updates the terminal dimensions.
func (h *Hub) SetTerminalDims(rows, cols uint16) {
	h.mu.Lock()
	defer h.mu.Unlock()

	h.TerminalDims.Rows = rows
	h.TerminalDims.Cols = cols

	// Resize all agents
	for _, ag := range h.Agents {
		ag.Resize(rows, cols)
	}
}

// TogglePolling enables/disables message polling.
func (h *Hub) TogglePolling() {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.PollingEnabled = !h.PollingEnabled
}

// RequestQuit signals the hub to shut down.
func (h *Hub) RequestQuit() {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.quit = true
}

// ShouldQuit returns true if the hub should shut down.
func (h *Hub) ShouldQuit() bool {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return h.quit
}

// Shutdown cleans up all resources.
func (h *Hub) Shutdown() error {
	h.Logger.Info("Shutting down hub")

	h.mu.Lock()
	defer h.mu.Unlock()

	// Close all agents
	for key, ag := range h.Agents {
		h.Logger.Info("Closing agent", "session_key", key)
		ag.Close()
	}

	// Close Tailnet connection
	if h.Tailnet != nil {
		h.Tailnet.Close()
	}

	return nil
}
