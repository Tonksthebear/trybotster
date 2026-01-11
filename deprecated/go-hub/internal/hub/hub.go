// Package hub provides the central state management for botster-hub.
//
// The Hub is the core orchestrator that owns all application state and
// coordinates between the TUI, server polling, and browser connectivity.
// It follows a centralized state store pattern where all state changes
// flow through the Hub.
package hub

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/trybotster/botster-hub/internal/agent"
	"github.com/trybotster/botster-hub/internal/config"
	"github.com/trybotster/botster-hub/internal/git"
	"github.com/trybotster/botster-hub/internal/notification"
	"github.com/trybotster/botster-hub/internal/server"
	"github.com/trybotster/botster-hub/internal/sshserver"
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

	// Server is the Rails API client.
	Server *server.Client

	// Git manages worktree operations.
	Git *git.Manager

	// SSHServer provides browser terminal access over tsnet.
	SSHServer *sshserver.Server

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

	// Initialize Rails API client
	if cfg.Token != "" {
		h.Server = server.New(&server.Config{
			BaseURL:  cfg.ServerURL,
			APIToken: cfg.Token,
			HubID:    hubID,
		}, logger)
	}

	// Initialize git manager with current working directory
	cwd, _ := os.Getwd()
	if cwd != "" {
		h.Git = git.New(cwd, logger)
	}

	return h, nil
}

// buildSessionKey creates a session key for deduplication checking.
// Matches agent.SessionKey() format: "owner-repo-42" for issues.
func buildSessionKey(repo string, issueNumber *int, branchName string) string {
	repoSafe := strings.ReplaceAll(repo, "/", "-")
	if issueNumber != nil {
		return fmt.Sprintf("%s-%d", repoSafe, *issueNumber)
	}
	branchSafe := strings.ReplaceAll(branchName, "/", "-")
	return fmt.Sprintf("%s-%s", repoSafe, branchSafe)
}

// generateHubID creates a stable hub identifier based on the repository path.
// This ensures the same repo always gets the same hub_id across CLI restarts,
// allowing Rails to reliably track hub identity and persist state.
func generateHubID() string {
	repoInfo, err := git.DetectCurrentRepo()
	if err != nil {
		// Fallback to UUID-like identifier if not in a repo
		return fmt.Sprintf("hub-%d", time.Now().UnixNano())
	}

	// SHA256 hash of canonical repo path for stable identifier
	hash := sha256.Sum256([]byte(repoInfo.Path))
	return hex.EncodeToString(hash[:16]) // Use first 16 bytes (32 hex chars)
}

// Setup performs initial hub setup (Tailnet connection, device registration).
// Tailnet connection happens async so the TUI can start immediately.
func (h *Hub) Setup(ctx context.Context) error {
	h.Logger.Info("Setting up hub", "hub_id", h.HubID)

	// Clean up any orphaned processes from previous runs
	h.cleanupOrphanedProcesses()

	// Register hub with server before subscribing to messages
	if err := h.registerHub(ctx); err != nil {
		h.Logger.Warn("Hub registration failed (will retry on heartbeat)", "error", err)
	}

	// Connect to Tailscale async if Headscale is configured
	if h.Config.HeadscaleURL != "" {
		go h.setupTailnet(ctx)
	}

	return nil
}

// cleanupOrphanedProcesses finds and cleans up processes left by previous crashed sessions.
func (h *Hub) cleanupOrphanedProcesses() {
	if h.Git == nil {
		return
	}

	worktrees, err := h.Git.ListAllWorktrees()
	if err != nil {
		h.Logger.Debug("Could not list worktrees for cleanup", "error", err)
		return
	}

	for _, wt := range worktrees {
		// Check for .botster_teardown file - indicates worktree should be deleted
		teardownPath := filepath.Join(wt.Path, ".botster_teardown")
		if _, err := os.Stat(teardownPath); err == nil {
			h.Logger.Info("Found orphaned worktree with teardown marker, cleaning up", "path", wt.Path)

			// Delete the worktree
			if err := h.Git.DeleteWorktreeByPath(wt.Path, wt.Branch); err != nil {
				h.Logger.Warn("Failed to cleanup orphaned worktree", "path", wt.Path, "error", err)
			}
		}
	}
}

// registerHub performs initial registration with the Rails server.
// This tells the server about this hub and its configuration.
func (h *Hub) registerHub(ctx context.Context) error {
	if h.Server == nil {
		return nil
	}

	// Get repository name for registration
	repoName := ""
	if h.Git != nil {
		if repoInfo, err := git.DetectCurrentRepo(); err == nil {
			repoName = repoInfo.Name
		}
	}

	// Send initial heartbeat with empty agents list
	_, err := h.Server.SendHeartbeat(ctx, repoName, []server.AgentHeartbeatInfo{})
	if err != nil {
		return err
	}

	h.Logger.Info("Hub registered with server", "repo", repoName)
	return nil
}

// setupTailnet connects to Tailscale in the background.
func (h *Hub) setupTailnet(ctx context.Context) {
	h.Logger.Info("Connecting to Headscale", "url", h.Config.HeadscaleURL)

	// Get pre-auth key from Rails server if we have a server client
	authKey := ""
	if h.Server != nil {
		key, err := h.Server.GetBrowserKey(ctx)
		if err != nil {
			h.Logger.Warn("Failed to get pre-auth key from server", "error", err)
			// Without an auth key, tsnet blocks forever waiting for manual approval
			return
		}
		authKey = key
	} else {
		h.Logger.Info("Skipping Tailscale setup (no server configured)")
		return
	}

	client, err := tailnet.New(&tailnet.Config{
		HubID:        h.HubID,
		HeadscaleURL: h.Config.HeadscaleURL,
		AuthKey:      authKey,
		Ephemeral:    true,
	}, h.Logger)

	if err != nil {
		h.Logger.Error("Failed to create Tailnet client", "error", err)
		return
	}

	if err := client.Start(ctx); err != nil {
		h.Logger.Warn("Failed to connect to Tailnet", "error", err)
		return
	}

	h.mu.Lock()
	h.Tailnet = client
	h.mu.Unlock()

	// Get connection URL for QR code
	ips := client.TailscaleIPs()
	if len(ips) > 0 {
		h.mu.Lock()
		h.ConnectionURL = fmt.Sprintf("ssh://user@%s", ips[0])
		h.mu.Unlock()

		// Update hostname in Rails
		if h.Server != nil {
			hostname := client.Hostname()
			if err := h.Server.UpdateHostname(ctx, hostname); err != nil {
				h.Logger.Warn("Failed to update hostname", "error", err)
			}
		}
	}

	// Start SSH server over tsnet
	sshListener, err := client.Listen("tcp", ":22")
	if err != nil {
		h.Logger.Error("Failed to listen on tsnet:22", "error", err)
		return
	}

	h.mu.Lock()
	h.SSHServer = sshserver.New(sshListener, h, h.Logger)
	h.mu.Unlock()

	go func() {
		if err := h.SSHServer.Serve(ctx); err != nil && ctx.Err() == nil {
			h.Logger.Error("SSH server error", "error", err)
		}
	}()

	h.Logger.Info("SSH server started on tsnet:22")
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

	// Collect and forward notifications from all agents
	h.collectAndForwardNotifications()
}

// pollMessages fetches new messages from the server.
func (h *Hub) pollMessages() {
	if h.Server == nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	messages, err := h.Server.PollMessages(ctx)
	if err != nil {
		h.Logger.Warn("Failed to poll messages", "error", err)
		return
	}

	for _, msg := range messages {
		h.handleMessage(msg)
	}
}

// handleMessage processes a message from the server.
func (h *Hub) handleMessage(msg server.Message) {
	h.Logger.Info("Received message",
		"id", msg.ID,
		"type", msg.EventType,
	)

	// TODO: Handle different message types
	// - issue_mention: spawn agent for issue
	// - webrtc_offer: handle browser connection (legacy)

	// Acknowledge the message
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := h.Server.AcknowledgeMessage(ctx, msg.ID); err != nil {
		h.Logger.Warn("Failed to acknowledge message", "id", msg.ID, "error", err)
	}
}

// sendHeartbeat registers the hub with the server.
func (h *Hub) sendHeartbeat() {
	if h.Server == nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := h.Server.Heartbeat(ctx); err != nil {
		h.Logger.Warn("Failed to send heartbeat", "error", err)
	}
}

// collectAndForwardNotifications drains notifications from all agents and sends them to Rails.
// This is called every tick to ensure timely notification delivery.
func (h *Hub) collectAndForwardNotifications() {
	if h.Server == nil {
		return
	}

	for _, ag := range h.Agents {
		// Non-blocking drain of notification channel
		for {
			select {
			case n := <-ag.Notifications():
				// Map notification type to API format
				notificationType := mapNotificationType(n)
				if notificationType == "" {
					continue
				}

				// Get invocation URL from agent (if available)
				var invocationURL *string
				// Note: Agent doesn't track invocation URL currently
				// This would need to be added if we want to include it

				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				err := h.Server.SendNotification(ctx, ag.Repo, ag.IssueNumber, invocationURL, notificationType)
				cancel()

				if err != nil {
					h.Logger.Warn("Failed to forward notification",
						"agent", ag.SessionKey(),
						"type", notificationType,
						"error", err,
					)
				} else {
					h.Logger.Info("Forwarded notification to Rails",
						"agent", ag.SessionKey(),
						"type", notificationType,
					)
				}
			default:
				// No more notifications in channel
				goto nextAgent
			}
		}
	nextAgent:
	}
}

// mapNotificationType converts a notification.Notification to the API type string.
func mapNotificationType(n notification.Notification) string {
	// Parse notification message for known types
	msg := n.Message
	if n.Type == notification.TypeOSC777 {
		msg = n.Title + " " + n.Body
	}

	msg = strings.ToLower(msg)

	// Map to known notification types that Rails expects
	switch {
	case strings.Contains(msg, "finished"):
		return "finished"
	case strings.Contains(msg, "failed"):
		return "failed"
	case strings.Contains(msg, "complete"):
		return "finished"
	case strings.Contains(msg, "error"):
		return "failed"
	default:
		// Generic notification - send as-is if it's meaningful
		if n.Message != "" {
			return "status"
		}
		return ""
	}
}

// SpawnAgent creates and starts a new agent with proper environment and init files.
// If an agent already exists for the same repo/issue, it returns nil (no-op).
func (h *Hub) SpawnAgent(repo string, issueNumber *int, branchName, worktreePath, command string, env map[string]string) error {
	h.mu.Lock()
	defer h.mu.Unlock()

	// Detect repo if not provided
	if repo == "" && h.Git != nil {
		repoInfo, err := git.DetectCurrentRepo()
		if err == nil {
			repo = repoInfo.Name
		}
	}

	// Check for existing agent (deduplication)
	sessionKey := buildSessionKey(repo, issueNumber, branchName)
	if _, exists := h.Agents[sessionKey]; exists {
		h.Logger.Info("Agent already exists for this issue, skipping spawn",
			"session_key", sessionKey,
			"repo", repo,
			"issue", issueNumber,
		)
		return nil
	}

	ag := agent.New(repo, issueNumber, branchName, worktreePath)

	// Build comprehensive environment variables (matching Rust)
	fullEnv := make(map[string]string)
	for k, v := range env {
		fullEnv[k] = v
	}

	// Add BOTSTER_* environment variables
	fullEnv["BOTSTER_REPO"] = repo
	if issueNumber != nil {
		fullEnv["BOTSTER_ISSUE_NUMBER"] = fmt.Sprintf("%d", *issueNumber)
	} else {
		fullEnv["BOTSTER_ISSUE_NUMBER"] = "0"
	}
	fullEnv["BOTSTER_BRANCH_NAME"] = branchName
	fullEnv["BOTSTER_WORKTREE_PATH"] = worktreePath

	// Get current executable path for BOTSTER_HUB_BIN
	if exe, err := os.Executable(); err == nil {
		fullEnv["BOTSTER_HUB_BIN"] = exe
	} else {
		fullEnv["BOTSTER_HUB_BIN"] = "botster-hub"
	}

	// Write .botster_prompt file with user's prompt
	if prompt, ok := env["PROMPT"]; ok && prompt != "" {
		promptPath := worktreePath + "/.botster_prompt"
		if err := os.WriteFile(promptPath, []byte(prompt), 0644); err != nil {
			h.Logger.Warn("Failed to write .botster_prompt", "error", err)
		}
	}

	// Copy .botster_init from main repo to worktree if it exists
	if h.Git != nil {
		repoInfo, err := git.DetectCurrentRepo()
		if err == nil {
			srcInit := repoInfo.Path + "/.botster_init"
			dstInit := worktreePath + "/.botster_init"
			if data, err := os.ReadFile(srcInit); err == nil {
				if err := os.WriteFile(dstInit, data, 0755); err != nil {
					h.Logger.Warn("Failed to copy .botster_init", "error", err)
				}
			}
		}
	}

	// Determine spawn command - use bash and source .botster_init if it exists
	spawnCommand := command
	initPath := worktreePath + "/.botster_init"
	if _, err := os.Stat(initPath); err == nil {
		// .botster_init exists, source it
		spawnCommand = "source .botster_init"
	}

	// Spawn with current terminal dimensions
	if err := ag.Spawn(spawnCommand, fullEnv, h.TerminalDims.Rows, h.TerminalDims.Cols); err != nil {
		return fmt.Errorf("failed to spawn agent: %w", err)
	}

	// Check for .botster_server and spawn server PTY if present
	serverPath := worktreePath + "/.botster_server"
	if data, err := os.ReadFile(serverPath); err == nil {
		serverCmd := string(data)
		if serverCmd != "" {
			// Allocate tunnel port
			tunnelPort := h.allocateTunnelPort()
			ag.TunnelPort = &tunnelPort
			fullEnv["BOTSTER_TUNNEL_PORT"] = fmt.Sprintf("%d", tunnelPort)

			if err := ag.SpawnServer(serverCmd, fullEnv, h.TerminalDims.Rows, h.TerminalDims.Cols); err != nil {
				h.Logger.Warn("Failed to spawn server PTY", "error", err)
			} else {
				h.Logger.Info("Server PTY spawned", "port", tunnelPort)
			}
		}
	}

	h.Agents[ag.SessionKey()] = ag
	h.Logger.Info("Agent spawned",
		"session_key", ag.SessionKey(),
		"repo", repo,
		"branch", branchName,
		"worktree", worktreePath,
	)

	return nil
}

// nextTunnelPort is used for allocating tunnel ports starting at 3000.
var nextTunnelPort = 3000

// allocateTunnelPort returns the next available tunnel port.
func (h *Hub) allocateTunnelPort() int {
	port := nextTunnelPort
	nextTunnelPort++
	return port
}

// CloseAgent terminates an agent (keeps worktree).
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

// CloseAgentAndDeleteWorktree terminates an agent and deletes its worktree.
func (h *Hub) CloseAgentAndDeleteWorktree(sessionKey string) error {
	h.mu.Lock()
	defer h.mu.Unlock()

	ag, ok := h.Agents[sessionKey]
	if !ok {
		return fmt.Errorf("agent not found: %s", sessionKey)
	}

	worktreePath := ag.WorktreePath
	branchName := ag.BranchName

	// Close the agent first
	if err := ag.Close(); err != nil {
		h.Logger.Warn("Error closing agent", "error", err)
	}
	delete(h.Agents, sessionKey)

	// Delete the worktree
	if h.Git != nil && worktreePath != "" {
		if err := h.Git.DeleteWorktreeByPath(worktreePath, branchName); err != nil {
			h.Logger.Error("Failed to delete worktree", "path", worktreePath, "error", err)
			return fmt.Errorf("failed to delete worktree: %w", err)
		}
		h.Logger.Info("Deleted worktree", "path", worktreePath)
	}

	h.Logger.Info("Agent closed and worktree deleted", "session_key", sessionKey)
	return nil
}

// getAgentsSorted returns agents in a stable order (sorted by start time).
// Must be called with lock held.
func (h *Hub) getAgentsSorted() []*agent.Agent {
	var agents []*agent.Agent
	for _, ag := range h.Agents {
		agents = append(agents, ag)
	}

	// Sort by start time for stable ordering
	for i := 0; i < len(agents)-1; i++ {
		for j := i + 1; j < len(agents); j++ {
			if agents[j].StartTime.Before(agents[i].StartTime) {
				agents[i], agents[j] = agents[j], agents[i]
			}
		}
	}

	return agents
}

// GetSelectedAgent returns the currently selected agent.
func (h *Hub) GetSelectedAgent() *agent.Agent {
	h.mu.RLock()
	defer h.mu.RUnlock()

	agents := h.getAgentsSorted()
	if len(agents) == 0 {
		return nil
	}

	if h.SelectedAgent >= len(agents) {
		h.SelectedAgent = 0
	}

	return agents[h.SelectedAgent]
}

// GetAgentsOrdered returns all agents in a stable order for display.
func (h *Hub) GetAgentsOrdered() []*agent.Agent {
	h.mu.RLock()
	defer h.mu.RUnlock()

	return h.getAgentsSorted()
}

// GetAvailableWorktrees returns worktrees that don't have active agents.
// This filters out worktrees where an agent is already running.
func (h *Hub) GetAvailableWorktrees() ([]*git.Worktree, error) {
	if h.Git == nil {
		return nil, fmt.Errorf("git manager not initialized")
	}

	allWorktrees, err := h.Git.ListAllWorktrees()
	if err != nil {
		return nil, err
	}

	// Build set of worktree paths with active agents
	h.mu.RLock()
	activeWorktrees := make(map[string]bool)
	for _, ag := range h.Agents {
		if ag.WorktreePath != "" {
			activeWorktrees[ag.WorktreePath] = true
		}
	}
	h.mu.RUnlock()

	// Filter out worktrees with active agents
	var available []*git.Worktree
	for _, wt := range allWorktrees {
		if !activeWorktrees[wt.Path] {
			available = append(available, wt)
		}
	}

	return available, nil
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

// SelectNextAgent moves selection to the next agent.
func (h *Hub) SelectNextAgent() {
	h.mu.Lock()
	defer h.mu.Unlock()

	if len(h.Agents) == 0 {
		return
	}

	h.SelectedAgent++
	if h.SelectedAgent >= len(h.Agents) {
		h.SelectedAgent = 0
	}
}

// SelectPreviousAgent moves selection to the previous agent.
func (h *Hub) SelectPreviousAgent() {
	h.mu.Lock()
	defer h.mu.Unlock()

	if len(h.Agents) == 0 {
		return
	}

	h.SelectedAgent--
	if h.SelectedAgent < 0 {
		h.SelectedAgent = len(h.Agents) - 1
	}
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

	// Notify server about shutdown (before taking lock)
	if h.Server != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		if err := h.notifyShutdown(ctx); err != nil {
			h.Logger.Warn("Failed to notify server of shutdown", "error", err)
		}
		cancel()
	}

	h.mu.Lock()
	defer h.mu.Unlock()

	// Close SSH server
	if h.SSHServer != nil {
		h.SSHServer.Close()
	}

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

// notifyShutdown tells the server this hub is going offline.
func (h *Hub) notifyShutdown(ctx context.Context) error {
	// A simple approach: send a final heartbeat that the server can detect
	// (or implement a dedicated shutdown endpoint if the Rails API supports it)
	// For now, just log it - the server will detect the hub is gone via heartbeat timeout
	h.Logger.Info("Hub shutdown notification sent")
	return nil
}

// --- SessionProvider interface (for sshserver) ---

// GetSession returns an agent session by ID.
func (h *Hub) GetSession(agentID string) (sshserver.AgentSession, bool) {
	h.mu.RLock()
	defer h.mu.RUnlock()

	for _, ag := range h.Agents {
		if ag.GetID() == agentID || ag.ID.String()[:8] == agentID {
			return ag, true
		}
	}
	return nil, false
}

// ListSessions returns all active session IDs.
func (h *Hub) ListSessions() []string {
	h.mu.RLock()
	defer h.mu.RUnlock()

	var ids []string
	for _, ag := range h.Agents {
		ids = append(ids, ag.ID.String()[:8]) // Short ID
	}
	return ids
}
