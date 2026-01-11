package hub

import (
	"testing"

	"github.com/trybotster/botster-hub/internal/agent"
	"github.com/trybotster/botster-hub/internal/git"
)

func TestGetAvailableWorktrees_NilGit(t *testing.T) {
	h := &Hub{
		Agents: make(map[string]*agent.Agent),
		Git:    nil,
	}

	_, err := h.GetAvailableWorktrees()
	if err == nil {
		t.Error("GetAvailableWorktrees should return error when Git is nil")
	}
}

func TestFilterWorktreesByActiveAgents(t *testing.T) {
	tests := []struct {
		name             string
		allWorktrees     []*git.Worktree
		activeWorktrees  map[string]bool
		expectedCount    int
		expectedPaths    []string
	}{
		{
			name: "no active agents",
			allWorktrees: []*git.Worktree{
				{Path: "/path/1", Branch: "feature-1"},
				{Path: "/path/2", Branch: "feature-2"},
			},
			activeWorktrees: map[string]bool{},
			expectedCount:   2,
			expectedPaths:   []string{"/path/1", "/path/2"},
		},
		{
			name: "one active agent",
			allWorktrees: []*git.Worktree{
				{Path: "/path/1", Branch: "feature-1"},
				{Path: "/path/2", Branch: "feature-2"},
			},
			activeWorktrees: map[string]bool{"/path/1": true},
			expectedCount:   1,
			expectedPaths:   []string{"/path/2"},
		},
		{
			name: "all worktrees have active agents",
			allWorktrees: []*git.Worktree{
				{Path: "/path/1", Branch: "feature-1"},
				{Path: "/path/2", Branch: "feature-2"},
			},
			activeWorktrees: map[string]bool{"/path/1": true, "/path/2": true},
			expectedCount:   0,
			expectedPaths:   []string{},
		},
		{
			name:             "empty worktree list",
			allWorktrees:     []*git.Worktree{},
			activeWorktrees:  map[string]bool{"/path/1": true},
			expectedCount:    0,
			expectedPaths:    []string{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := filterWorktreesByActiveAgents(tt.allWorktrees, tt.activeWorktrees)

			if len(result) != tt.expectedCount {
				t.Errorf("len(result) = %d, want %d", len(result), tt.expectedCount)
			}

			for i, wt := range result {
				if i < len(tt.expectedPaths) && wt.Path != tt.expectedPaths[i] {
					t.Errorf("result[%d].Path = %q, want %q", i, wt.Path, tt.expectedPaths[i])
				}
			}
		})
	}
}

// filterWorktreesByActiveAgents is a pure function for testing the filtering logic.
// This matches the logic in GetAvailableWorktrees.
func filterWorktreesByActiveAgents(all []*git.Worktree, active map[string]bool) []*git.Worktree {
	var available []*git.Worktree
	for _, wt := range all {
		if !active[wt.Path] {
			available = append(available, wt)
		}
	}
	return available
}
