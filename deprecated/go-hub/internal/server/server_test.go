package server

import (
	"context"
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
)

func testLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError}))
}

// === Message Payload Extraction Tests ===

func TestMessageRepo(t *testing.T) {
	tests := []struct {
		name    string
		payload map[string]interface{}
		want    string
	}{
		{
			name: "repository.full_name",
			payload: map[string]interface{}{
				"repository": map[string]interface{}{
					"full_name": "owner/repo",
				},
			},
			want: "owner/repo",
		},
		{
			name: "flat repo",
			payload: map[string]interface{}{
				"repo": "owner/repo",
			},
			want: "owner/repo",
		},
		{
			name:    "missing",
			payload: map[string]interface{}{},
			want:    "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msg := Message{Payload: tt.payload}
			if got := msg.Repo(); got != tt.want {
				t.Errorf("Repo() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestMessageIssueNumber(t *testing.T) {
	tests := []struct {
		name    string
		payload map[string]interface{}
		want    *int
	}{
		{
			name: "flat issue_number",
			payload: map[string]interface{}{
				"issue_number": float64(42),
			},
			want: intPtr(42),
		},
		{
			name: "nested issue.number",
			payload: map[string]interface{}{
				"issue": map[string]interface{}{
					"number": float64(123),
				},
			},
			want: intPtr(123),
		},
		{
			name: "pull_request.number",
			payload: map[string]interface{}{
				"pull_request": map[string]interface{}{
					"number": float64(456),
				},
			},
			want: intPtr(456),
		},
		{
			name:    "missing",
			payload: map[string]interface{}{},
			want:    nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msg := Message{Payload: tt.payload}
			got := msg.IssueNumber()
			if tt.want == nil && got != nil {
				t.Errorf("IssueNumber() = %v, want nil", *got)
			} else if tt.want != nil && (got == nil || *got != *tt.want) {
				t.Errorf("IssueNumber() = %v, want %v", got, *tt.want)
			}
		})
	}
}

func TestMessagePrompt(t *testing.T) {
	tests := []struct {
		name    string
		payload map[string]interface{}
		want    string
	}{
		{
			name:    "prompt field",
			payload: map[string]interface{}{"prompt": "Fix the bug"},
			want:    "Fix the bug",
		},
		{
			name:    "context fallback",
			payload: map[string]interface{}{"context": "Some context"},
			want:    "Some context",
		},
		{
			name:    "missing",
			payload: map[string]interface{}{},
			want:    "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msg := Message{Payload: tt.payload}
			if got := msg.Prompt(); got != tt.want {
				t.Errorf("Prompt() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestMessageInvocationURL(t *testing.T) {
	msg := Message{
		Payload: map[string]interface{}{
			"issue_url": "https://github.com/owner/repo/issues/42",
		},
	}

	if got := msg.InvocationURL(); got != "https://github.com/owner/repo/issues/42" {
		t.Errorf("InvocationURL() = %q", got)
	}

	emptyMsg := Message{Payload: map[string]interface{}{}}
	if got := emptyMsg.InvocationURL(); got != "" {
		t.Errorf("InvocationURL() for empty = %q, want empty", got)
	}
}

func TestMessageCommentAuthor(t *testing.T) {
	msg := Message{
		Payload: map[string]interface{}{
			"comment_author": "alice",
		},
	}

	if got := msg.CommentAuthor(); got != "alice" {
		t.Errorf("CommentAuthor() = %q, want 'alice'", got)
	}
}

func TestMessageCommentBody(t *testing.T) {
	msg := Message{
		Payload: map[string]interface{}{
			"comment_body": "Please fix this bug",
		},
	}

	if got := msg.CommentBody(); got != "Please fix this bug" {
		t.Errorf("CommentBody() = %q", got)
	}
}

func TestMessageIsCleanup(t *testing.T) {
	cleanupMsg := Message{EventType: "agent_cleanup"}
	if !cleanupMsg.IsCleanup() {
		t.Error("IsCleanup() should be true for agent_cleanup")
	}

	normalMsg := Message{EventType: "issue_comment"}
	if normalMsg.IsCleanup() {
		t.Error("IsCleanup() should be false for issue_comment")
	}
}

func TestMessageIsWebRTCOffer(t *testing.T) {
	webrtcMsg := Message{EventType: "webrtc_offer"}
	if !webrtcMsg.IsWebRTCOffer() {
		t.Error("IsWebRTCOffer() should be true for webrtc_offer")
	}

	normalMsg := Message{EventType: "issue_comment"}
	if normalMsg.IsWebRTCOffer() {
		t.Error("IsWebRTCOffer() should be false for issue_comment")
	}
}

// === HTTP Client Tests ===

func TestPollMessages(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "GET" {
			t.Errorf("Method = %q, want GET", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/messages" {
			t.Errorf("Path = %q", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer test-token" {
			t.Errorf("Authorization = %q", r.Header.Get("Authorization"))
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(MessagesResponse{
			Messages: []Message{
				{ID: 1, EventType: "issue_comment", Payload: map[string]interface{}{}},
				{ID: 2, EventType: "pull_request", Payload: map[string]interface{}{}},
			},
			Count: 2,
		})
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	messages, err := client.PollMessages(context.Background())
	if err != nil {
		t.Fatalf("PollMessages failed: %v", err)
	}

	if len(messages) != 2 {
		t.Errorf("len(messages) = %d, want 2", len(messages))
	}
	if messages[0].ID != 1 {
		t.Errorf("messages[0].ID = %d, want 1", messages[0].ID)
	}
}

func TestPollMessagesError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		w.Write([]byte("Internal Server Error"))
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	_, err := client.PollMessages(context.Background())
	if err == nil {
		t.Error("PollMessages should fail on 500")
	}
}

func TestAcknowledgeMessage(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PATCH" {
			t.Errorf("Method = %q, want PATCH", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/messages/42" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	err := client.AcknowledgeMessage(context.Background(), 42)
	if err != nil {
		t.Fatalf("AcknowledgeMessage failed: %v", err)
	}
}

func TestHeartbeat(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PATCH" {
			t.Errorf("Method = %q, want PATCH", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/heartbeat" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	err := client.Heartbeat(context.Background())
	if err != nil {
		t.Fatalf("Heartbeat failed: %v", err)
	}
}

func TestSendHeartbeat(t *testing.T) {
	var receivedPayload HeartbeatPayload

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PUT" {
			t.Errorf("Method = %q, want PUT", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub" {
			t.Errorf("Path = %q", r.URL.Path)
		}
		if r.Header.Get("Content-Type") != "application/json" {
			t.Errorf("Content-Type = %q", r.Header.Get("Content-Type"))
		}

		json.NewDecoder(r.Body).Decode(&receivedPayload)
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	invURL := "https://example.com/invoke"
	agents := []AgentHeartbeatInfo{
		{SessionKey: "owner-repo-42", LastInvocationURL: &invURL},
		{SessionKey: "owner-repo-43", LastInvocationURL: nil},
	}

	success, err := client.SendHeartbeat(context.Background(), "owner/repo", agents)
	if err != nil {
		t.Fatalf("SendHeartbeat failed: %v", err)
	}
	if !success {
		t.Error("SendHeartbeat should return true on success")
	}

	if receivedPayload.Repo != "owner/repo" {
		t.Errorf("Payload.Repo = %q, want 'owner/repo'", receivedPayload.Repo)
	}
	if len(receivedPayload.Agents) != 2 {
		t.Errorf("len(Payload.Agents) = %d, want 2", len(receivedPayload.Agents))
	}
}

func TestSendHeartbeatFailure(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadGateway)
		w.Write([]byte("Bad Gateway"))
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	success, err := client.SendHeartbeat(context.Background(), "owner/repo", nil)
	if err != nil {
		t.Fatalf("SendHeartbeat should not return error on HTTP failure: %v", err)
	}
	if success {
		t.Error("SendHeartbeat should return false on failure")
	}
}

func TestSendNotification(t *testing.T) {
	var receivedPayload NotificationPayload

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" {
			t.Errorf("Method = %q, want POST", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/notifications" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		json.NewDecoder(r.Body).Decode(&receivedPayload)
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	issueNum := 42
	invURL := "https://github.com/owner/repo/issues/42"
	err := client.SendNotification(context.Background(), "owner/repo", &issueNum, &invURL, "question_asked")
	if err != nil {
		t.Fatalf("SendNotification failed: %v", err)
	}

	if receivedPayload.Repo != "owner/repo" {
		t.Errorf("Payload.Repo = %q", receivedPayload.Repo)
	}
	if receivedPayload.IssueNumber == nil || *receivedPayload.IssueNumber != 42 {
		t.Errorf("Payload.IssueNumber = %v, want 42", receivedPayload.IssueNumber)
	}
	if receivedPayload.NotificationType != "question_asked" {
		t.Errorf("Payload.NotificationType = %q", receivedPayload.NotificationType)
	}
}

func TestSendNotificationError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		w.Write([]byte("Unauthorized"))
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "bad-token",
		HubID:    "test-hub",
	}, testLogger())

	err := client.SendNotification(context.Background(), "owner/repo", nil, nil, "question_asked")
	if err == nil {
		t.Error("SendNotification should fail on 401")
	}
}

func TestGetBrowserKey(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" {
			t.Errorf("Method = %q, want POST", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/tailscale/browser_key" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(BrowserKeyResponse{Key: "tskey-client-abc123"})
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	key, err := client.GetBrowserKey(context.Background())
	if err != nil {
		t.Fatalf("GetBrowserKey failed: %v", err)
	}
	if key != "tskey-client-abc123" {
		t.Errorf("Key = %q, want 'tskey-client-abc123'", key)
	}
}

func TestUpdateHostname(t *testing.T) {
	var receivedHostname string

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PATCH" {
			t.Errorf("Method = %q, want PATCH", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/tailscale/hostname" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		var body map[string]string
		json.NewDecoder(r.Body).Decode(&body)
		receivedHostname = body["hostname"]

		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	err := client.UpdateHostname(context.Background(), "botster-hub-abc123")
	if err != nil {
		t.Fatalf("UpdateHostname failed: %v", err)
	}
	if receivedHostname != "botster-hub-abc123" {
		t.Errorf("Hostname = %q, want 'botster-hub-abc123'", receivedHostname)
	}
}

func TestGetTailscaleStatus(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "GET" {
			t.Errorf("Method = %q, want GET", r.Method)
		}
		if r.URL.Path != "/hubs/test-hub/tailscale/status" {
			t.Errorf("Path = %q", r.URL.Path)
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(TailscaleStatus{
			Connected:         true,
			Hostname:          "botster-hub-xyz",
			PreauthKeyPresent: true,
		})
	}))
	defer server.Close()

	client := New(&Config{
		BaseURL:  server.URL,
		APIToken: "test-token",
		HubID:    "test-hub",
	}, testLogger())

	status, err := client.GetTailscaleStatus(context.Background())
	if err != nil {
		t.Fatalf("GetTailscaleStatus failed: %v", err)
	}
	if !status.Connected {
		t.Error("Connected should be true")
	}
	if status.Hostname != "botster-hub-xyz" {
		t.Errorf("Hostname = %q", status.Hostname)
	}
	if !status.PreauthKeyPresent {
		t.Error("PreauthKeyPresent should be true")
	}
}

// === Type Tests ===

func TestAgentHeartbeatInfoJSON(t *testing.T) {
	invURL := "https://example.com/invoke"
	info := AgentHeartbeatInfo{
		SessionKey:        "owner-repo-42",
		LastInvocationURL: &invURL,
	}

	data, err := json.Marshal(info)
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]interface{}
	json.Unmarshal(data, &decoded)

	if decoded["session_key"] != "owner-repo-42" {
		t.Errorf("session_key = %v", decoded["session_key"])
	}
	if decoded["last_invocation_url"] != "https://example.com/invoke" {
		t.Errorf("last_invocation_url = %v", decoded["last_invocation_url"])
	}
}

func TestAgentHeartbeatInfoOmitEmpty(t *testing.T) {
	info := AgentHeartbeatInfo{
		SessionKey:        "owner-repo-42",
		LastInvocationURL: nil,
	}

	data, err := json.Marshal(info)
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]interface{}
	json.Unmarshal(data, &decoded)

	if _, exists := decoded["last_invocation_url"]; exists {
		t.Error("last_invocation_url should be omitted when nil")
	}
}

func TestNotificationPayloadJSON(t *testing.T) {
	issueNum := 42
	invURL := "https://github.com/owner/repo/issues/42"
	payload := NotificationPayload{
		Repo:             "owner/repo",
		IssueNumber:      &issueNum,
		InvocationURL:    &invURL,
		NotificationType: "question_asked",
	}

	data, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]interface{}
	json.Unmarshal(data, &decoded)

	if decoded["repo"] != "owner/repo" {
		t.Errorf("repo = %v", decoded["repo"])
	}
	if decoded["notification_type"] != "question_asked" {
		t.Errorf("notification_type = %v", decoded["notification_type"])
	}
}

func TestNotificationPayloadOmitEmpty(t *testing.T) {
	payload := NotificationPayload{
		Repo:             "owner/repo",
		NotificationType: "completed",
	}

	data, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]interface{}
	json.Unmarshal(data, &decoded)

	if _, exists := decoded["issue_number"]; exists {
		t.Error("issue_number should be omitted when nil")
	}
	if _, exists := decoded["invocation_url"]; exists {
		t.Error("invocation_url should be omitted when nil")
	}
}

// Helper function
func intPtr(n int) *int {
	return &n
}
