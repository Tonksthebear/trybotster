package hub

import (
	"testing"

	"github.com/trybotster/botster-hub/internal/agent"
)

func createTestAgent(repo string, issueNumber *int, branchName string) *agent.Agent {
	return agent.New(repo, issueNumber, branchName, "/tmp/worktree")
}

func TestNewHubState(t *testing.T) {
	state := NewHubState()

	if !state.IsEmpty() {
		t.Error("New state should be empty")
	}
	if state.AgentCount() != 0 {
		t.Errorf("AgentCount() = %d, want 0", state.AgentCount())
	}
	if state.SelectedIndex() != 0 {
		t.Errorf("SelectedIndex() = %d, want 0", state.SelectedIndex())
	}
}

func TestAddAgent(t *testing.T) {
	state := NewHubState()

	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	if state.IsEmpty() {
		t.Error("State should not be empty after adding agent")
	}
	if state.AgentCount() != 1 {
		t.Errorf("AgentCount() = %d, want 1", state.AgentCount())
	}

	// Should be able to retrieve by key
	retrieved, ok := state.GetAgent("owner-repo-42")
	if !ok {
		t.Error("Should be able to get agent by key")
	}
	if retrieved != ag {
		t.Error("Retrieved agent should match added agent")
	}
}

func TestRemoveAgent(t *testing.T) {
	state := NewHubState()

	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	removed := state.RemoveAgent("owner-repo-42")
	if removed == nil {
		t.Error("RemoveAgent should return the removed agent")
	}
	if removed != ag {
		t.Error("Removed agent should match added agent")
	}

	if !state.IsEmpty() {
		t.Error("State should be empty after removing only agent")
	}

	// Removing non-existent should return nil
	removed = state.RemoveAgent("nonexistent")
	if removed != nil {
		t.Error("RemoveAgent should return nil for non-existent key")
	}
}

func TestSelectedAgent(t *testing.T) {
	state := NewHubState()

	// Empty state returns nil
	if state.SelectedAgent() != nil {
		t.Error("SelectedAgent should return nil for empty state")
	}

	// Add agent
	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	// Should return the added agent
	selected := state.SelectedAgent()
	if selected != ag {
		t.Error("SelectedAgent should return the first agent")
	}
}

func TestSelectedSessionKey(t *testing.T) {
	state := NewHubState()

	// Empty state returns empty string
	if state.SelectedSessionKey() != "" {
		t.Errorf("SelectedSessionKey() = %q, want empty", state.SelectedSessionKey())
	}

	// Add agent
	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	if state.SelectedSessionKey() != "owner-repo-42" {
		t.Errorf("SelectedSessionKey() = %q, want 'owner-repo-42'", state.SelectedSessionKey())
	}
}

func TestSelectNext(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	// Initially at 0
	if state.SelectedIndex() != 0 {
		t.Errorf("SelectedIndex() = %d, want 0", state.SelectedIndex())
	}

	// Move forward
	state.SelectNext()
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, want 1", state.SelectedIndex())
	}

	state.SelectNext()
	if state.SelectedIndex() != 2 {
		t.Errorf("SelectedIndex() = %d, want 2", state.SelectedIndex())
	}

	// Wrap around
	state.SelectNext()
	if state.SelectedIndex() != 0 {
		t.Errorf("SelectedIndex() = %d, want 0 (wrap around)", state.SelectedIndex())
	}
}

func TestSelectPrevious(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	// Wrap backwards from 0
	state.SelectPrevious()
	if state.SelectedIndex() != 2 {
		t.Errorf("SelectedIndex() = %d, want 2 (wrap backwards)", state.SelectedIndex())
	}

	state.SelectPrevious()
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, want 1", state.SelectedIndex())
	}
}

func TestSelectByIndex(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	// 1-based indexing
	if !state.SelectByIndex(2) {
		t.Error("SelectByIndex(2) should succeed")
	}
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, want 1", state.SelectedIndex())
	}

	// Out of bounds returns false
	if state.SelectByIndex(0) {
		t.Error("SelectByIndex(0) should fail")
	}
	if state.SelectByIndex(5) {
		t.Error("SelectByIndex(5) should fail")
	}

	// Selection should not have changed
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, should not have changed", state.SelectedIndex())
	}
}

func TestSelectByKey(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	if !state.SelectByKey("key-2") {
		t.Error("SelectByKey('key-2') should succeed")
	}
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, want 1", state.SelectedIndex())
	}

	// Non-existent key
	if state.SelectByKey("nonexistent") {
		t.Error("SelectByKey('nonexistent') should fail")
	}
}

func TestSelectionClampsOnRemove(t *testing.T) {
	state := NewHubState()

	// Add three agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	// Select last agent
	state.SelectByIndex(3)
	if state.SelectedIndex() != 2 {
		t.Errorf("SelectedIndex() = %d, want 2", state.SelectedIndex())
	}

	// Remove last agent
	state.RemoveAgent("key-3")

	// Selection should clamp to new max
	if state.SelectedIndex() != 1 {
		t.Errorf("SelectedIndex() = %d, want 1 (clamped)", state.SelectedIndex())
	}
}

func TestAgentsOrdered(t *testing.T) {
	state := NewHubState()

	// Add agents in specific order
	keys := []string{"agent-a", "agent-b", "agent-c"}
	for _, key := range keys {
		ag := createTestAgent("owner/repo", nil, "main")
		state.AddAgent(key, ag)
	}

	ordered := state.AgentsOrdered()
	if len(ordered) != 3 {
		t.Fatalf("len(ordered) = %d, want 3", len(ordered))
	}

	// Verify order preserved
	for i, pair := range ordered {
		if pair.SessionKey != keys[i] {
			t.Errorf("ordered[%d].SessionKey = %q, want %q", i, pair.SessionKey, keys[i])
		}
	}
}

func TestSnapshot(t *testing.T) {
	state := NewHubState()

	// Empty snapshot
	snap := state.Snapshot()
	if !snap.IsEmpty {
		t.Error("Snapshot.IsEmpty should be true for empty state")
	}
	if snap.AgentCount != 0 {
		t.Errorf("Snapshot.AgentCount = %d, want 0", snap.AgentCount)
	}

	// Add agent
	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	snap = state.Snapshot()
	if snap.IsEmpty {
		t.Error("Snapshot.IsEmpty should be false")
	}
	if snap.AgentCount != 1 {
		t.Errorf("Snapshot.AgentCount = %d, want 1", snap.AgentCount)
	}
	if snap.SessionKey != "owner-repo-42" {
		t.Errorf("Snapshot.SessionKey = %q, want 'owner-repo-42'", snap.SessionKey)
	}
	if len(snap.Agents) != 1 {
		t.Errorf("len(Snapshot.Agents) = %d, want 1", len(snap.Agents))
	}
}

func TestAvailableWorktrees(t *testing.T) {
	state := NewHubState()

	// Initially empty
	if len(state.AvailableWorktrees()) != 0 {
		t.Error("AvailableWorktrees should be empty initially")
	}

	// Set worktrees
	worktrees := []WorktreeInfo{
		{Path: "/path/1", Branch: "feature-1"},
		{Path: "/path/2", Branch: "feature-2"},
	}
	state.SetAvailableWorktrees(worktrees)

	available := state.AvailableWorktrees()
	if len(available) != 2 {
		t.Errorf("len(AvailableWorktrees) = %d, want 2", len(available))
	}

	// Clear
	state.ClearAvailableWorktrees()
	if len(state.AvailableWorktrees()) != 0 {
		t.Error("AvailableWorktrees should be empty after clear")
	}
}

func TestEmptyStateNavigation(t *testing.T) {
	state := NewHubState()

	// Navigation on empty state should not panic
	state.SelectNext()
	state.SelectPrevious()

	if state.SelectedIndex() != 0 {
		t.Errorf("SelectedIndex() = %d, want 0", state.SelectedIndex())
	}
}

func TestSafeHubState(t *testing.T) {
	safeState := NewSafeHubState()

	// Add agent via WithWrite
	safeState.WithWrite(func(state *HubState) {
		issueNum := 42
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
		state.AddAgent("owner-repo-42", ag)
	})

	// Read via WithRead
	var count int
	safeState.WithRead(func(state *HubState) {
		count = state.AgentCount()
	})

	if count != 1 {
		t.Errorf("AgentCount = %d, want 1", count)
	}

	// Snapshot should work
	snap := safeState.Snapshot()
	if snap.AgentCount != 1 {
		t.Errorf("Snapshot.AgentCount = %d, want 1", snap.AgentCount)
	}
}

func TestAllAgents(t *testing.T) {
	state := NewHubState()

	// Add agents
	for i := 1; i <= 3; i++ {
		issueNum := i
		ag := createTestAgent("owner/repo", &issueNum, "botster-issue")
		state.AddAgent("key-"+string(rune('0'+i)), ag)
	}

	all := state.AllAgents()
	if len(all) != 3 {
		t.Errorf("len(AllAgents) = %d, want 3", len(all))
	}
}

func TestWorktreeInfoStruct(t *testing.T) {
	info := WorktreeInfo{
		Path:   "/path/to/worktree",
		Branch: "botster-feature",
	}

	if info.Path != "/path/to/worktree" {
		t.Errorf("Path = %q", info.Path)
	}
	if info.Branch != "botster-feature" {
		t.Errorf("Branch = %q", info.Branch)
	}
}

func TestAgentSnapshotStruct(t *testing.T) {
	state := NewHubState()

	issueNum := 42
	ag := createTestAgent("owner/repo", &issueNum, "botster-issue-42")
	state.AddAgent("owner-repo-42", ag)

	snap := state.Snapshot()
	if len(snap.Agents) != 1 {
		t.Fatal("Expected one agent in snapshot")
	}

	agSnap := snap.Agents[0]
	if agSnap.SessionKey != "owner-repo-42" {
		t.Errorf("SessionKey = %q", agSnap.SessionKey)
	}
	if agSnap.Repo != "owner/repo" {
		t.Errorf("Repo = %q", agSnap.Repo)
	}
	if agSnap.Branch != "botster-issue-42" {
		t.Errorf("Branch = %q", agSnap.Branch)
	}
}
