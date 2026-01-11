// Package hub provides agent lifecycle management.
//
// This file contains functions for spawning and closing agents within the Hub.
package hub

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/trybotster/botster-hub/internal/agent"
	"github.com/trybotster/botster-hub/internal/tunnel"
)

// AgentSpawnConfig contains configuration for spawning a new agent.
type AgentSpawnConfig struct {
	// Issue number this agent is working on (if any).
	IssueNumber *uint32
	// Branch name for the agent's worktree.
	BranchName string
	// Path to the git worktree directory.
	WorktreePath string
	// Path to the main repository.
	RepoPath string
	// Repository name in "owner/repo" format.
	RepoName string
	// Initial prompt to send to the agent.
	Prompt string
	// Server message ID that triggered this spawn (if any).
	MessageID *int64
	// Invocation URL for tracking this agent instance.
	InvocationURL string
}

// NewAgentSpawnConfig creates a new spawn configuration with required fields.
func NewAgentSpawnConfig(
	issueNumber *uint32,
	branchName string,
	worktreePath string,
	repoPath string,
	repoName string,
	prompt string,
) *AgentSpawnConfig {
	return &AgentSpawnConfig{
		IssueNumber:  issueNumber,
		BranchName:   branchName,
		WorktreePath: worktreePath,
		RepoPath:     repoPath,
		RepoName:     repoName,
		Prompt:       prompt,
	}
}

// WithMessageID sets the message ID and returns the config for chaining.
func (c *AgentSpawnConfig) WithMessageID(messageID int64) *AgentSpawnConfig {
	c.MessageID = &messageID
	return c
}

// WithInvocationURL sets the invocation URL and returns the config for chaining.
func (c *AgentSpawnConfig) WithInvocationURL(url string) *AgentSpawnConfig {
	c.InvocationURL = url
	return c
}

// SessionKey generates the session key for this agent.
func (c *AgentSpawnConfig) SessionKey() string {
	return GenerateSessionKey(c.RepoName, c.IssueNumber, c.BranchName)
}

// SpawnResult contains information about a spawned agent.
type SpawnResult struct {
	// The session key for the spawned agent.
	SessionKey string
	// The allocated tunnel port, if any.
	TunnelPort *uint16
	// Whether a server PTY was spawned.
	HasServerPTY bool
}

// SpawnAgent spawns a new agent with the given configuration.
//
// This function:
// 1. Creates a new Agent instance
// 2. Sets up environment variables
// 3. Writes the prompt file
// 4. Copies the init script
// 5. Spawns the CLI PTY
// 6. Optionally spawns a server PTY
// 7. Registers the agent with HubState
func SpawnAgent(
	state *HubState,
	config *AgentSpawnConfig,
	rows, cols uint16,
) (*SpawnResult, error) {
	// Convert issue number to *int for agent package
	var issueNum *int
	if config.IssueNumber != nil {
		n := int(*config.IssueNumber)
		issueNum = &n
	}

	ag := agent.New(
		config.RepoName,
		issueNum,
		config.BranchName,
		config.WorktreePath,
	)

	// Resize to terminal dimensions
	ag.Resize(rows, cols)

	// Write prompt to .botster_prompt file
	promptFilePath := filepath.Join(config.WorktreePath, ".botster_prompt")
	if err := os.WriteFile(promptFilePath, []byte(config.Prompt), 0644); err != nil {
		return nil, fmt.Errorf("failed to write .botster_prompt file: %w", err)
	}

	// Copy fresh .botster_init from main repo to worktree
	sourceInit := filepath.Join(config.RepoPath, ".botster_init")
	destInit := filepath.Join(config.WorktreePath, ".botster_init")
	if _, err := os.Stat(sourceInit); err == nil {
		data, err := os.ReadFile(sourceInit)
		if err != nil {
			return nil, fmt.Errorf("failed to read .botster_init: %w", err)
		}
		if err := os.WriteFile(destInit, data, 0755); err != nil {
			return nil, fmt.Errorf("failed to copy .botster_init to worktree: %w", err)
		}
	}

	// Build environment variables
	envVars := buildSpawnEnvironment(config)

	// Allocate a tunnel port for this agent
	var tunnelPort *uint16
	if port, err := tunnel.AllocateTunnelPort(); err == nil {
		tunnelPort = &port
		envVars["BOTSTER_TUNNEL_PORT"] = fmt.Sprintf("%d", port)
	}

	// Build spawn command with init
	spawnCommand := "source .botster_init"

	// Spawn the agent with correct dimensions
	if err := ag.Spawn(spawnCommand, envVars, rows, cols); err != nil {
		return nil, fmt.Errorf("failed to spawn agent: %w", err)
	}

	// Store tunnel port on the agent
	if tunnelPort != nil {
		port := int(*tunnelPort)
		ag.TunnelPort = &port
	}

	// Spawn server PTY if tunnel port is allocated and .botster_server exists
	hasServerPTY := false
	if tunnelPort != nil {
		hasServerPTY = spawnServerPTYIfExists(ag, config.WorktreePath, *tunnelPort, rows, cols)
	}

	// Register the agent
	sessionKey := ag.SessionKey()
	state.AddAgent(sessionKey, ag)

	return &SpawnResult{
		SessionKey:   sessionKey,
		TunnelPort:   tunnelPort,
		HasServerPTY: hasServerPTY,
	}, nil
}

// CloseAgent closes an agent and optionally deletes its worktree.
//
// Returns true if the agent was found and closed, false if not found.
func CloseAgent(
	state *HubState,
	sessionKey string,
	deleteWorktree bool,
) (bool, error) {
	ag := state.RemoveAgent(sessionKey)
	if ag == nil {
		return false, nil
	}

	// If deleting worktree, that would be handled by the caller
	// since HubState doesn't have direct access to git operations
	if deleteWorktree {
		// The caller should handle worktree deletion
		// This function just removes from state
	}

	return true, nil
}

// buildSpawnEnvironment builds environment variables for agent spawn.
func buildSpawnEnvironment(config *AgentSpawnConfig) map[string]string {
	envVars := make(map[string]string)

	envVars["BOTSTER_REPO"] = config.RepoName

	if config.IssueNumber != nil {
		envVars["BOTSTER_ISSUE_NUMBER"] = fmt.Sprintf("%d", *config.IssueNumber)
	} else {
		envVars["BOTSTER_ISSUE_NUMBER"] = "0"
	}

	envVars["BOTSTER_BRANCH_NAME"] = config.BranchName
	envVars["BOTSTER_WORKTREE_PATH"] = config.WorktreePath
	envVars["BOTSTER_TASK_DESCRIPTION"] = config.Prompt

	if config.MessageID != nil {
		envVars["BOTSTER_MESSAGE_ID"] = fmt.Sprintf("%d", *config.MessageID)
	}

	// Add the hub binary path for subprocesses
	if binPath, err := os.Executable(); err == nil {
		envVars["BOTSTER_HUB_BIN"] = binPath
	} else {
		envVars["BOTSTER_HUB_BIN"] = "botster-hub"
	}

	return envVars
}

// spawnServerPTYIfExists spawns a server PTY if .botster_server exists.
func spawnServerPTYIfExists(ag *agent.Agent, worktreePath string, port uint16, rows, cols uint16) bool {
	serverScript := filepath.Join(worktreePath, ".botster_server")
	if _, err := os.Stat(serverScript); os.IsNotExist(err) {
		return false
	}

	serverEnv := map[string]string{
		"BOTSTER_TUNNEL_PORT":   fmt.Sprintf("%d", port),
		"BOTSTER_WORKTREE_PATH": worktreePath,
	}

	if err := ag.SpawnServer(".botster_server", serverEnv, rows, cols); err != nil {
		return false
	}

	return true
}

// --- Session Key Generation ---

// GenerateSessionKey generates a unique session key from the given parameters.
//
// Format: "{repo-safe}-{identifier}" where identifier is either
// the issue number or a sanitized branch name.
func GenerateSessionKey(repoName string, issueNumber *uint32, branchName string) string {
	repoSafe := SanitizeRepoName(repoName)

	if issueNumber != nil {
		return fmt.Sprintf("%s-%d", repoSafe, *issueNumber)
	}
	return fmt.Sprintf("%s-%s", repoSafe, SanitizeBranchName(branchName))
}

// SanitizeRepoName sanitizes a repository name for use in a session key.
// Replaces "/" with "-" to create a safe identifier.
func SanitizeRepoName(repoName string) string {
	return strings.ReplaceAll(repoName, "/", "-")
}

// SanitizeBranchName sanitizes a branch name for use in a session key.
// Replaces "/" with "-" to create a safe identifier.
func SanitizeBranchName(branchName string) string {
	return strings.ReplaceAll(branchName, "/", "-")
}

// ExtractIssueNumber extracts issue number from a session key if present.
// Returns nil if the key doesn't end with a number.
func ExtractIssueNumber(sessionKey string) *uint32 {
	parts := strings.Split(sessionKey, "-")
	if len(parts) == 0 {
		return nil
	}

	lastPart := parts[len(parts)-1]
	var num uint32
	if _, err := fmt.Sscanf(lastPart, "%d", &num); err == nil {
		return &num
	}
	return nil
}

// FormatAgentLabel formats a human-readable label for an agent.
func FormatAgentLabel(issueNumber *uint32, branchName string) string {
	if issueNumber != nil {
		return fmt.Sprintf("issue #%d", *issueNumber)
	}
	return fmt.Sprintf("branch %s", branchName)
}
