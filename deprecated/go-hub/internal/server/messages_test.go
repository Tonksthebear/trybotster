package server

import (
	"testing"
)

func makeMessage(id int64, eventType string, payload map[string]interface{}) *Message {
	return &Message{
		ID:        id,
		EventType: eventType,
		Payload:   payload,
	}
}

func TestFromMessage(t *testing.T) {
	msg := makeMessage(1, "issue_comment", map[string]interface{}{
		"issue_number":   float64(42),
		"prompt":         "Fix the bug",
		"issue_url":      "https://github.com/owner/repo/issues/42",
		"comment_author": "alice",
		"comment_body":   "Please fix this",
	})

	parsed := FromMessage(msg)

	if parsed.MessageID != 1 {
		t.Errorf("MessageID = %d, want 1", parsed.MessageID)
	}
	if parsed.EventType != "issue_comment" {
		t.Errorf("EventType = %q, want 'issue_comment'", parsed.EventType)
	}
	if parsed.IssueNumber == nil || *parsed.IssueNumber != 42 {
		t.Errorf("IssueNumber = %v, want 42", parsed.IssueNumber)
	}
	if parsed.Prompt != "Fix the bug" {
		t.Errorf("Prompt = %q", parsed.Prompt)
	}
	if parsed.InvocationURL != "https://github.com/owner/repo/issues/42" {
		t.Errorf("InvocationURL = %q", parsed.InvocationURL)
	}
	if parsed.CommentAuthor != "alice" {
		t.Errorf("CommentAuthor = %q", parsed.CommentAuthor)
	}
}

func TestParsedMessageWithNestedIssue(t *testing.T) {
	msg := makeMessage(2, "issue_comment", map[string]interface{}{
		"issue": map[string]interface{}{
			"number": float64(123),
		},
		"repository": map[string]interface{}{
			"full_name": "owner/repo",
		},
	})

	parsed := FromMessage(msg)

	if parsed.IssueNumber == nil || *parsed.IssueNumber != 123 {
		t.Errorf("IssueNumber = %v, want 123", parsed.IssueNumber)
	}
	if parsed.Repo != "owner/repo" {
		t.Errorf("Repo = %q", parsed.Repo)
	}
}

func TestParsedMessageIsCleanup(t *testing.T) {
	msg := makeMessage(3, "agent_cleanup", map[string]interface{}{
		"repo":         "owner/repo",
		"issue_number": float64(42),
	})

	parsed := FromMessage(msg)

	if !parsed.IsCleanup() {
		t.Error("IsCleanup() should be true")
	}
	if parsed.IsWebRTCOffer() {
		t.Error("IsWebRTCOffer() should be false")
	}
}

func TestParsedMessageIsWebRTCOffer(t *testing.T) {
	msg := makeMessage(4, "webrtc_offer", map[string]interface{}{
		"sdp": "offer...",
	})

	parsed := FromMessage(msg)

	if parsed.IsCleanup() {
		t.Error("IsCleanup() should be false")
	}
	if !parsed.IsWebRTCOffer() {
		t.Error("IsWebRTCOffer() should be true")
	}
}

func TestFormatNotificationWithPrompt(t *testing.T) {
	msg := makeMessage(1, "issue_comment", map[string]interface{}{
		"prompt": "Please review this PR",
	})

	parsed := FromMessage(msg)
	notification := parsed.FormatNotification()

	if notification == "" {
		t.Error("Notification should not be empty")
	}
	if !contains(notification, "NEW MENTION") {
		t.Error("Should contain 'NEW MENTION'")
	}
	if !contains(notification, "Please review this PR") {
		t.Error("Should contain prompt text")
	}
}

func TestFormatNotificationWithoutPrompt(t *testing.T) {
	msg := makeMessage(1, "issue_comment", map[string]interface{}{
		"comment_author": "alice",
		"comment_body":   "Hey bot, help!",
	})

	parsed := FromMessage(msg)
	notification := parsed.FormatNotification()

	if !contains(notification, "NEW MENTION") {
		t.Error("Should contain 'NEW MENTION'")
	}
	if !contains(notification, "alice") {
		t.Error("Should contain author name")
	}
	if !contains(notification, "Hey bot, help!") {
		t.Error("Should contain comment body")
	}
}

func TestFormatNotificationDefaults(t *testing.T) {
	msg := makeMessage(1, "issue_comment", map[string]interface{}{})

	parsed := FromMessage(msg)
	notification := parsed.FormatNotification()

	if !contains(notification, "unknown") {
		t.Error("Should use 'unknown' for missing author")
	}
	if !contains(notification, "New mention") {
		t.Error("Should use 'New mention' for missing body")
	}
}

func TestTaskDescription(t *testing.T) {
	tests := []struct {
		name    string
		payload map[string]interface{}
		want    string
	}{
		{
			name:    "with prompt",
			payload: map[string]interface{}{"prompt": "Fix the bug"},
			want:    "Fix the bug",
		},
		{
			name:    "with comment_body",
			payload: map[string]interface{}{"comment_body": "Please help"},
			want:    "Please help",
		},
		{
			name:    "fallback",
			payload: map[string]interface{}{},
			want:    "Work on this issue",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msg := makeMessage(1, "issue_comment", tt.payload)
			parsed := FromMessage(msg)
			if got := parsed.TaskDescription(); got != tt.want {
				t.Errorf("TaskDescription() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestSessionKeyFromMessage(t *testing.T) {
	tests := []struct {
		repo  string
		issue int
		want  string
	}{
		{"owner/repo", 42, "owner-repo-42"},
		{"org/project", 123, "org-project-123"},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			if got := SessionKeyFromMessage(tt.repo, tt.issue); got != tt.want {
				t.Errorf("SessionKeyFromMessage(%q, %d) = %q, want %q", tt.repo, tt.issue, got, tt.want)
			}
		})
	}
}

// Helper
func contains(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
