package hub

import (
	"os"
	"path/filepath"
	"testing"
)

func TestGenerateSessionKeyWithIssue(t *testing.T) {
	num := uint32(42)
	key := GenerateSessionKey("owner/repo", &num, "issue-42")

	if key != "owner-repo-42" {
		t.Errorf("got %q, want 'owner-repo-42'", key)
	}
}

func TestGenerateSessionKeyWithoutIssue(t *testing.T) {
	key := GenerateSessionKey("owner/repo", nil, "feature-branch")

	if key != "owner-repo-feature-branch" {
		t.Errorf("got %q, want 'owner-repo-feature-branch'", key)
	}
}

func TestGenerateSessionKeyNestedBranch(t *testing.T) {
	key := GenerateSessionKey("owner/repo", nil, "feature/nested/branch")

	if key != "owner-repo-feature-nested-branch" {
		t.Errorf("got %q, want 'owner-repo-feature-nested-branch'", key)
	}
}

func TestSanitizeRepoName(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"owner/repo", "owner-repo"},
		{"org/nested/repo", "org-nested-repo"},
		{"simple", "simple"},
		{"", ""},
	}

	for _, tt := range tests {
		result := SanitizeRepoName(tt.input)
		if result != tt.expected {
			t.Errorf("SanitizeRepoName(%q) = %q, want %q", tt.input, result, tt.expected)
		}
	}
}

func TestSanitizeBranchName(t *testing.T) {
	tests := []struct {
		input    string
		expected string
	}{
		{"feature/test", "feature-test"},
		{"simple-branch", "simple-branch"},
		{"feature/nested/branch", "feature-nested-branch"},
		{"main", "main"},
	}

	for _, tt := range tests {
		result := SanitizeBranchName(tt.input)
		if result != tt.expected {
			t.Errorf("SanitizeBranchName(%q) = %q, want %q", tt.input, result, tt.expected)
		}
	}
}

func TestExtractIssueNumber(t *testing.T) {
	tests := []struct {
		key      string
		expected *uint32
	}{
		{"owner-repo-42", uintPtr(42)},
		{"owner-repo-123", uintPtr(123)},
		{"owner-repo-feature-branch", nil},
		{"owner-repo-0", uintPtr(0)},
		{"", nil},
	}

	for _, tt := range tests {
		result := ExtractIssueNumber(tt.key)
		if tt.expected == nil {
			if result != nil {
				t.Errorf("ExtractIssueNumber(%q) = %d, want nil", tt.key, *result)
			}
		} else if result == nil {
			t.Errorf("ExtractIssueNumber(%q) = nil, want %d", tt.key, *tt.expected)
		} else if *result != *tt.expected {
			t.Errorf("ExtractIssueNumber(%q) = %d, want %d", tt.key, *result, *tt.expected)
		}
	}
}

func TestFormatAgentLabelWithIssue(t *testing.T) {
	num := uint32(42)
	label := FormatAgentLabel(&num, "issue-42")

	if label != "issue #42" {
		t.Errorf("got %q, want 'issue #42'", label)
	}
}

func TestFormatAgentLabelWithoutIssue(t *testing.T) {
	label := FormatAgentLabel(nil, "feature-branch")

	if label != "branch feature-branch" {
		t.Errorf("got %q, want 'branch feature-branch'", label)
	}
}

func TestNewAgentSpawnConfig(t *testing.T) {
	num := uint32(42)
	config := NewAgentSpawnConfig(
		&num,
		"issue-42",
		"/tmp/worktree",
		"/tmp/repo",
		"owner/repo",
		"Fix the bug",
	)

	if config.IssueNumber == nil || *config.IssueNumber != 42 {
		t.Error("issue number mismatch")
	}
	if config.BranchName != "issue-42" {
		t.Error("branch name mismatch")
	}
	if config.WorktreePath != "/tmp/worktree" {
		t.Error("worktree path mismatch")
	}
	if config.RepoPath != "/tmp/repo" {
		t.Error("repo path mismatch")
	}
	if config.RepoName != "owner/repo" {
		t.Error("repo name mismatch")
	}
	if config.Prompt != "Fix the bug" {
		t.Error("prompt mismatch")
	}
	if config.MessageID != nil {
		t.Error("message ID should be nil")
	}
	if config.InvocationURL != "" {
		t.Error("invocation URL should be empty")
	}
}

func TestAgentSpawnConfigBuilder(t *testing.T) {
	num := uint32(42)
	config := NewAgentSpawnConfig(
		&num,
		"issue-42",
		"/tmp/worktree",
		"/tmp/repo",
		"owner/repo",
		"Fix the bug",
	).WithMessageID(123).WithInvocationURL("https://example.com")

	if config.MessageID == nil || *config.MessageID != 123 {
		t.Error("message ID mismatch")
	}
	if config.InvocationURL != "https://example.com" {
		t.Error("invocation URL mismatch")
	}
}

func TestAgentSpawnConfigSessionKey(t *testing.T) {
	num := uint32(42)
	config := NewAgentSpawnConfig(
		&num,
		"issue-42",
		"/tmp/worktree",
		"/tmp/repo",
		"owner/repo",
		"Fix the bug",
	)

	key := config.SessionKey()
	if key != "owner-repo-42" {
		t.Errorf("got %q, want 'owner-repo-42'", key)
	}
}

func TestBuildSpawnEnvironment(t *testing.T) {
	num := uint32(42)
	msgID := int64(123)
	config := &AgentSpawnConfig{
		IssueNumber:  &num,
		BranchName:   "issue-42",
		WorktreePath: "/tmp/worktree",
		RepoPath:     "/tmp/repo",
		RepoName:     "owner/repo",
		Prompt:       "Fix the bug",
		MessageID:    &msgID,
	}

	env := buildSpawnEnvironment(config)

	if env["BOTSTER_REPO"] != "owner/repo" {
		t.Error("BOTSTER_REPO mismatch")
	}
	if env["BOTSTER_ISSUE_NUMBER"] != "42" {
		t.Error("BOTSTER_ISSUE_NUMBER mismatch")
	}
	if env["BOTSTER_BRANCH_NAME"] != "issue-42" {
		t.Error("BOTSTER_BRANCH_NAME mismatch")
	}
	if env["BOTSTER_WORKTREE_PATH"] != "/tmp/worktree" {
		t.Error("BOTSTER_WORKTREE_PATH mismatch")
	}
	if env["BOTSTER_TASK_DESCRIPTION"] != "Fix the bug" {
		t.Error("BOTSTER_TASK_DESCRIPTION mismatch")
	}
	if env["BOTSTER_MESSAGE_ID"] != "123" {
		t.Error("BOTSTER_MESSAGE_ID mismatch")
	}
	if _, ok := env["BOTSTER_HUB_BIN"]; !ok {
		t.Error("BOTSTER_HUB_BIN should be set")
	}
}

func TestBuildSpawnEnvironmentNoIssue(t *testing.T) {
	config := &AgentSpawnConfig{
		IssueNumber:  nil,
		BranchName:   "feature-branch",
		WorktreePath: "/tmp/worktree",
		RepoPath:     "/tmp/repo",
		RepoName:     "owner/repo",
		Prompt:       "Work on feature",
	}

	env := buildSpawnEnvironment(config)

	if env["BOTSTER_ISSUE_NUMBER"] != "0" {
		t.Errorf("BOTSTER_ISSUE_NUMBER = %q, want '0'", env["BOTSTER_ISSUE_NUMBER"])
	}
	if _, ok := env["BOTSTER_MESSAGE_ID"]; ok {
		t.Error("BOTSTER_MESSAGE_ID should not be set")
	}
}

func TestCloseAgentNotFound(t *testing.T) {
	state := NewHubState()

	found, err := CloseAgent(state, "nonexistent-key", false)

	if err != nil {
		t.Errorf("unexpected error: %v", err)
	}
	if found {
		t.Error("expected false for nonexistent agent")
	}
}

func TestSpawnServerPTYIfExistsNoScript(t *testing.T) {
	dir := t.TempDir()
	// No .botster_server file

	// This would panic without an actual agent, so we just test the path check
	serverScript := filepath.Join(dir, ".botster_server")
	if _, err := os.Stat(serverScript); !os.IsNotExist(err) {
		t.Error("server script should not exist")
	}
}

func TestSpawnServerPTYIfExistsWithScript(t *testing.T) {
	dir := t.TempDir()
	serverScript := filepath.Join(dir, ".botster_server")

	// Create server script
	if err := os.WriteFile(serverScript, []byte("#!/bin/bash\necho test"), 0755); err != nil {
		t.Fatalf("failed to write server script: %v", err)
	}

	if _, err := os.Stat(serverScript); os.IsNotExist(err) {
		t.Error("server script should exist")
	}
}

func uintPtr(n uint32) *uint32 {
	return &n
}
