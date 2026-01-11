package hub

import (
	"testing"

	"github.com/trybotster/botster-hub/internal/server"
)

func makeParsedMessage(id int64, eventType string, payload map[string]interface{}) *server.ParsedMessage {
	msg := &server.Message{
		ID:        id,
		EventType: eventType,
		Payload:   payload,
	}
	return server.FromMessage(msg)
}

func defaultMessageContext() *MessageContext {
	return &MessageContext{
		RepoPath:          "/home/user/repo",
		RepoName:          "owner/repo",
		WorktreeBase:      "/tmp/worktrees",
		MaxSessions:       10,
		CurrentAgentCount: 0,
	}
}

func TestMessageToHubActionSpawn(t *testing.T) {
	parsed := makeParsedMessage(1, "issue_comment", map[string]interface{}{
		"issue_number": float64(42),
		"prompt":       "Fix the bug",
	})

	ctx := defaultMessageContext()
	action, err := MessageToHubAction(parsed, ctx)

	if err != nil {
		t.Fatalf("Unexpected error: %v", err)
	}
	if action == nil {
		t.Fatal("Action should not be nil")
	}

	if action.Type != ActionSpawnAgent {
		t.Errorf("Type = %v, want ActionSpawnAgent", action.Type)
	}
	if action.IssueNumber == nil || *action.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v, want 42", action.IssueNumber)
	}
	if action.BranchName != "botster-issue-42" {
		t.Errorf("BranchName = %q, want 'botster-issue-42'", action.BranchName)
	}
	if action.MessageID == nil || *action.MessageID != 1 {
		t.Errorf("MessageID = %v, want 1", action.MessageID)
	}
	if action.WorktreePath != "/tmp/worktrees/botster-issue-42" {
		t.Errorf("WorktreePath = %q", action.WorktreePath)
	}
	if action.RepoPath != "/home/user/repo" {
		t.Errorf("RepoPath = %q", action.RepoPath)
	}
	if action.RepoName != "owner/repo" {
		t.Errorf("RepoName = %q", action.RepoName)
	}
}

func TestMessageToHubActionCleanup(t *testing.T) {
	parsed := makeParsedMessage(2, "agent_cleanup", map[string]interface{}{
		"repo":         "owner/repo",
		"issue_number": float64(42),
	})

	ctx := defaultMessageContext()
	action, err := MessageToHubAction(parsed, ctx)

	if err != nil {
		t.Fatalf("Unexpected error: %v", err)
	}
	if action == nil {
		t.Fatal("Action should not be nil")
	}

	if action.Type != ActionCloseAgent {
		t.Errorf("Type = %v, want ActionCloseAgent", action.Type)
	}
	if action.SessionKey != "owner-repo-42" {
		t.Errorf("SessionKey = %q, want 'owner-repo-42'", action.SessionKey)
	}
	if action.DeleteWorktree {
		t.Error("DeleteWorktree should be false for cleanup")
	}
}

func TestMessageToHubActionWebRTCReturnsNil(t *testing.T) {
	parsed := makeParsedMessage(3, "webrtc_offer", map[string]interface{}{
		"sdp": "offer...",
	})

	ctx := defaultMessageContext()
	action, err := MessageToHubAction(parsed, ctx)

	if err != nil {
		t.Fatalf("Unexpected error: %v", err)
	}
	if action != nil {
		t.Error("Action should be nil for webrtc_offer")
	}
}

func TestMessageToHubActionMaxSessions(t *testing.T) {
	parsed := makeParsedMessage(1, "issue_comment", map[string]interface{}{
		"issue_number": float64(42),
	})

	ctx := &MessageContext{
		CurrentAgentCount: 10,
		MaxSessions:       10,
		RepoPath:          "/home/user/repo",
		RepoName:          "owner/repo",
		WorktreeBase:      "/tmp/worktrees",
	}

	action, err := MessageToHubAction(parsed, ctx)

	if err == nil {
		t.Error("Expected error for max sessions")
	}
	if err.Kind != ErrMaxSessionsReached {
		t.Errorf("Error kind = %v, want ErrMaxSessionsReached", err.Kind)
	}
	if action != nil {
		t.Error("Action should be nil on error")
	}
}

func TestMessageToHubActionMissingIssueNumber(t *testing.T) {
	parsed := makeParsedMessage(1, "issue_comment", map[string]interface{}{
		"prompt": "Do something",
	})

	ctx := defaultMessageContext()

	action, err := MessageToHubAction(parsed, ctx)

	if err == nil {
		t.Error("Expected error for missing issue_number")
	}
	if err.Kind != ErrMissingField {
		t.Errorf("Error kind = %v, want ErrMissingField", err.Kind)
	}
	if err.Field != "issue_number" {
		t.Errorf("Error field = %q, want 'issue_number'", err.Field)
	}
	if action != nil {
		t.Error("Action should be nil on error")
	}
}

func TestMessageToHubActionCleanupMissingRepo(t *testing.T) {
	parsed := makeParsedMessage(1, "agent_cleanup", map[string]interface{}{
		"issue_number": float64(42),
	})

	ctx := defaultMessageContext()

	action, err := MessageToHubAction(parsed, ctx)

	if err == nil {
		t.Error("Expected error for missing repo")
	}
	if err.Kind != ErrMissingField {
		t.Errorf("Error kind = %v, want ErrMissingField", err.Kind)
	}
	if err.Field != "repo" {
		t.Errorf("Error field = %q, want 'repo'", err.Field)
	}
	if action != nil {
		t.Error("Action should be nil on error")
	}
}

func TestMessageToHubActionCleanupMissingIssueNumber(t *testing.T) {
	parsed := makeParsedMessage(1, "agent_cleanup", map[string]interface{}{
		"repo": "owner/repo",
	})

	ctx := defaultMessageContext()

	action, err := MessageToHubAction(parsed, ctx)

	if err == nil {
		t.Error("Expected error for missing issue_number")
	}
	if err.Field != "issue_number" {
		t.Errorf("Error field = %q, want 'issue_number'", err.Field)
	}
	if action != nil {
		t.Error("Action should be nil on error")
	}
}

func TestMessageErrorDisplay(t *testing.T) {
	missingErr := MissingFieldError("issue_number")
	if missingErr.Error() != "missing required field: issue_number" {
		t.Errorf("MissingFieldError.Error() = %q", missingErr.Error())
	}

	maxErr := MaxSessionsError(10)
	expected := "maximum concurrent sessions (10) reached"
	if maxErr.Error() != expected {
		t.Errorf("MaxSessionsError.Error() = %q, want %q", maxErr.Error(), expected)
	}
}

func TestMessageContextFields(t *testing.T) {
	ctx := &MessageContext{
		RepoPath:          "/path/to/repo",
		RepoName:          "owner/repo",
		WorktreeBase:      "/tmp/worktrees",
		MaxSessions:       5,
		CurrentAgentCount: 3,
	}

	if ctx.RepoPath != "/path/to/repo" {
		t.Errorf("RepoPath = %q", ctx.RepoPath)
	}
	if ctx.RepoName != "owner/repo" {
		t.Errorf("RepoName = %q", ctx.RepoName)
	}
	if ctx.MaxSessions != 5 {
		t.Errorf("MaxSessions = %d", ctx.MaxSessions)
	}
	if ctx.CurrentAgentCount != 3 {
		t.Errorf("CurrentAgentCount = %d", ctx.CurrentAgentCount)
	}
}

func TestMessageToHubActionWithInvocationURL(t *testing.T) {
	parsed := makeParsedMessage(1, "issue_comment", map[string]interface{}{
		"issue_number": float64(42),
		"issue_url":    "https://github.com/owner/repo/issues/42",
	})

	ctx := defaultMessageContext()
	action, err := MessageToHubAction(parsed, ctx)

	if err != nil {
		t.Fatalf("Unexpected error: %v", err)
	}

	if action.InvocationURL != "https://github.com/owner/repo/issues/42" {
		t.Errorf("InvocationURL = %q", action.InvocationURL)
	}
}
