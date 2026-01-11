// Package hub provides the central state management for botster-hub.
//
// This file contains the HubState type which manages the core state
// for active agents and selection, separate from the Hub's orchestration logic.
package hub

import (
	"sync"

	"github.com/trybotster/botster-hub/internal/agent"
)

// HubState manages the core agent state with ordered navigation.
//
// This type maintains both a map for O(1) lookups and an ordered slice
// for consistent UI navigation. All state modifications go through
// methods to ensure consistency between the two data structures.
//
// Thread-safety is provided by the Hub's mutex - this type is not
// independently thread-safe.
type HubState struct {
	// agents maps session keys to active agents.
	// Session keys are formatted as "{owner}-{repo}-{issue}" or "{owner}-{repo}-{branch}".
	agents map[string]*agent.Agent

	// agentKeysOrdered maintains insertion order for UI navigation.
	agentKeysOrdered []string

	// selected is the index into agentKeysOrdered for the currently selected agent.
	// Will be clamped to valid range when agents are added or removed.
	selected int

	// availableWorktrees lists worktrees available for spawning new agents.
	// Each tuple is (path, branch_name). Excludes worktrees with active agents.
	availableWorktrees []WorktreeInfo
}

// WorktreeInfo represents an available worktree for spawning.
type WorktreeInfo struct {
	Path   string
	Branch string
}

// NewHubState creates a new HubState.
func NewHubState() *HubState {
	return &HubState{
		agents:             make(map[string]*agent.Agent),
		agentKeysOrdered:   make([]string, 0),
		selected:           0,
		availableWorktrees: make([]WorktreeInfo, 0),
	}
}

// AgentCount returns the number of active agents.
func (s *HubState) AgentCount() int {
	return len(s.agents)
}

// IsEmpty returns true if there are no active agents.
func (s *HubState) IsEmpty() bool {
	return len(s.agents) == 0
}

// AddAgent adds an agent to the state.
// The agent will be added to both the map and the ordered list.
func (s *HubState) AddAgent(sessionKey string, ag *agent.Agent) {
	s.agentKeysOrdered = append(s.agentKeysOrdered, sessionKey)
	s.agents[sessionKey] = ag
}

// RemoveAgent removes an agent from the state.
// Returns the removed agent if it existed.
func (s *HubState) RemoveAgent(sessionKey string) *agent.Agent {
	// Remove from ordered list
	for i, key := range s.agentKeysOrdered {
		if key == sessionKey {
			s.agentKeysOrdered = append(s.agentKeysOrdered[:i], s.agentKeysOrdered[i+1:]...)
			break
		}
	}

	ag, existed := s.agents[sessionKey]
	if existed {
		delete(s.agents, sessionKey)
	}

	// Clamp selection to valid range
	if len(s.agentKeysOrdered) == 0 {
		s.selected = 0
	} else if s.selected >= len(s.agentKeysOrdered) {
		s.selected = len(s.agentKeysOrdered) - 1
	}

	return ag
}

// GetAgent returns an agent by session key.
func (s *HubState) GetAgent(sessionKey string) (*agent.Agent, bool) {
	ag, ok := s.agents[sessionKey]
	return ag, ok
}

// SelectedAgent returns the currently selected agent, if any.
func (s *HubState) SelectedAgent() *agent.Agent {
	if len(s.agentKeysOrdered) == 0 {
		return nil
	}
	if s.selected >= len(s.agentKeysOrdered) {
		s.selected = 0
	}
	key := s.agentKeysOrdered[s.selected]
	return s.agents[key]
}

// SelectedSessionKey returns the session key of the currently selected agent.
// Returns empty string if no agents.
func (s *HubState) SelectedSessionKey() string {
	if len(s.agentKeysOrdered) == 0 {
		return ""
	}
	if s.selected >= len(s.agentKeysOrdered) {
		s.selected = 0
	}
	return s.agentKeysOrdered[s.selected]
}

// SelectedIndex returns the current selection index (0-based).
func (s *HubState) SelectedIndex() int {
	return s.selected
}

// SelectNext selects the next agent (wraps around).
func (s *HubState) SelectNext() {
	if len(s.agentKeysOrdered) == 0 {
		return
	}
	s.selected = (s.selected + 1) % len(s.agentKeysOrdered)
}

// SelectPrevious selects the previous agent (wraps around).
func (s *HubState) SelectPrevious() {
	if len(s.agentKeysOrdered) == 0 {
		return
	}
	if s.selected == 0 {
		s.selected = len(s.agentKeysOrdered) - 1
	} else {
		s.selected--
	}
}

// SelectByIndex selects an agent by index (1-based for keyboard shortcuts).
// Returns true if the selection was valid.
func (s *HubState) SelectByIndex(index int) bool {
	// 1-based indexing for keyboard shortcuts (press 1 for first agent)
	if index < 1 || index > len(s.agentKeysOrdered) {
		return false
	}
	s.selected = index - 1
	return true
}

// SelectByKey selects an agent by session key.
// Returns true if the agent was found and selected.
func (s *HubState) SelectByKey(sessionKey string) bool {
	for i, key := range s.agentKeysOrdered {
		if key == sessionKey {
			s.selected = i
			return true
		}
	}
	return false
}

// AgentsOrdered returns all agents in display order as (sessionKey, agent) pairs.
func (s *HubState) AgentsOrdered() []struct {
	SessionKey string
	Agent      *agent.Agent
} {
	result := make([]struct {
		SessionKey string
		Agent      *agent.Agent
	}, 0, len(s.agentKeysOrdered))

	for _, key := range s.agentKeysOrdered {
		if ag, ok := s.agents[key]; ok {
			result = append(result, struct {
				SessionKey string
				Agent      *agent.Agent
			}{SessionKey: key, Agent: ag})
		}
	}

	return result
}

// AllAgents returns all agents (unordered, for iteration).
func (s *HubState) AllAgents() map[string]*agent.Agent {
	return s.agents
}

// AvailableWorktrees returns the list of worktrees available for spawning.
func (s *HubState) AvailableWorktrees() []WorktreeInfo {
	return s.availableWorktrees
}

// SetAvailableWorktrees sets the list of available worktrees.
func (s *HubState) SetAvailableWorktrees(worktrees []WorktreeInfo) {
	s.availableWorktrees = worktrees
}

// ClearAvailableWorktrees clears the available worktrees list.
func (s *HubState) ClearAvailableWorktrees() {
	s.availableWorktrees = s.availableWorktrees[:0]
}

// --- Thread-safe wrapper methods ---
// These methods are for use by the Hub to provide thread-safe access.

// StateSnapshot captures the current state for rendering.
type StateSnapshot struct {
	AgentCount  int
	Selected    int
	SessionKey  string
	Agents      []AgentSnapshot
	IsEmpty     bool
}

// AgentSnapshot captures an agent's state for rendering.
type AgentSnapshot struct {
	SessionKey string
	ID         string
	Status     string
	Age        string
	Repo       string
	Branch     string
}

// Snapshot returns a copy of the current state for thread-safe rendering.
func (s *HubState) Snapshot() StateSnapshot {
	snap := StateSnapshot{
		AgentCount: len(s.agents),
		Selected:   s.selected,
		SessionKey: s.SelectedSessionKey(),
		IsEmpty:    len(s.agents) == 0,
		Agents:     make([]AgentSnapshot, 0, len(s.agentKeysOrdered)),
	}

	for _, key := range s.agentKeysOrdered {
		if ag, ok := s.agents[key]; ok {
			snap.Agents = append(snap.Agents, AgentSnapshot{
				SessionKey: key,
				ID:         ag.GetID(),
				Status:     string(ag.Status),
				Age:        ag.Age().String(),
				Repo:       ag.Repo,
				Branch:     ag.BranchName,
			})
		}
	}

	return snap
}

// --- Concurrent-safe HubState wrapper ---

// SafeHubState wraps HubState with a mutex for thread-safe access.
type SafeHubState struct {
	state *HubState
	mu    sync.RWMutex
}

// NewSafeHubState creates a new thread-safe HubState wrapper.
func NewSafeHubState() *SafeHubState {
	return &SafeHubState{
		state: NewHubState(),
	}
}

// WithRead executes a function with read access to the state.
func (s *SafeHubState) WithRead(fn func(*HubState)) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	fn(s.state)
}

// WithWrite executes a function with write access to the state.
func (s *SafeHubState) WithWrite(fn func(*HubState)) {
	s.mu.Lock()
	defer s.mu.Unlock()
	fn(s.state)
}

// Snapshot returns a thread-safe snapshot of the current state.
func (s *SafeHubState) Snapshot() StateSnapshot {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.state.Snapshot()
}
