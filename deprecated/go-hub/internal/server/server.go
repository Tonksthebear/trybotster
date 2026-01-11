// Package server provides a client for the Botster Rails API.
//
// This package handles all communication with the Rails server:
//   - Polling for pending messages (GitHub webhook events)
//   - Acknowledging processed messages
//   - Heartbeat to keep hub online
//   - Tailscale integration (browser keys, hostname updates)
package server

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"time"
)

// Client is a client for the Botster Rails API.
type Client struct {
	baseURL    string
	apiToken   string
	hubID      string
	httpClient *http.Client
	logger     *slog.Logger
}

// Config holds configuration for the API client.
type Config struct {
	BaseURL  string // e.g., "https://trybotster.com"
	APIToken string // DeviceToken from Rails
	HubID    string // Hub identifier
}

// New creates a new API client.
func New(cfg *Config, logger *slog.Logger) *Client {
	return &Client{
		baseURL:  cfg.BaseURL,
		apiToken: cfg.APIToken,
		hubID:    cfg.HubID,
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
		logger: logger,
	}
}

// Message represents a message from the Rails server.
type Message struct {
	ID        int64                  `json:"id"`
	EventType string                 `json:"event_type"`
	Payload   map[string]interface{} `json:"payload"`
	CreatedAt time.Time              `json:"created_at"`
	SentAt    *time.Time             `json:"sent_at"`
	ClaimedAt *time.Time             `json:"claimed_at"`
}

// Repo extracts the repository name from the payload.
// Returns empty string if not found.
func (m *Message) Repo() string {
	// Try payload.repository.full_name first
	if repo, ok := m.Payload["repository"].(map[string]interface{}); ok {
		if fullName, ok := repo["full_name"].(string); ok {
			return fullName
		}
	}
	// Fall back to payload.repo
	if repo, ok := m.Payload["repo"].(string); ok {
		return repo
	}
	return ""
}

// IssueNumber extracts the issue number from the payload.
// Works for both issue events and pull request events.
func (m *Message) IssueNumber() *int {
	// Try issue_number first (flat)
	if num, ok := m.Payload["issue_number"].(float64); ok {
		n := int(num)
		return &n
	}
	// Try issue.number
	if issue, ok := m.Payload["issue"].(map[string]interface{}); ok {
		if num, ok := issue["number"].(float64); ok {
			n := int(num)
			return &n
		}
	}
	// Fall back to pull_request.number
	if pr, ok := m.Payload["pull_request"].(map[string]interface{}); ok {
		if num, ok := pr["number"].(float64); ok {
			n := int(num)
			return &n
		}
	}
	return nil
}

// Prompt extracts the task prompt from the payload.
func (m *Message) Prompt() string {
	if prompt, ok := m.Payload["prompt"].(string); ok {
		return prompt
	}
	if context, ok := m.Payload["context"].(string); ok {
		return context
	}
	return ""
}

// InvocationURL extracts the invocation URL from the payload.
func (m *Message) InvocationURL() string {
	if url, ok := m.Payload["issue_url"].(string); ok {
		return url
	}
	return ""
}

// CommentAuthor extracts the comment author from the payload.
func (m *Message) CommentAuthor() string {
	if author, ok := m.Payload["comment_author"].(string); ok {
		return author
	}
	return ""
}

// CommentBody extracts the comment body from the payload.
func (m *Message) CommentBody() string {
	if body, ok := m.Payload["comment_body"].(string); ok {
		return body
	}
	return ""
}

// IsCleanup returns true if this is a cleanup message.
func (m *Message) IsCleanup() bool {
	return m.EventType == "agent_cleanup"
}

// IsWebRTCOffer returns true if this is a WebRTC offer message.
func (m *Message) IsWebRTCOffer() bool {
	return m.EventType == "webrtc_offer"
}

// MessagesResponse is the response from GET /hubs/:id/messages.
type MessagesResponse struct {
	Messages []Message `json:"messages"`
	Count    int       `json:"count"`
}

// PollMessages fetches pending messages for this hub.
func (c *Client) PollMessages(ctx context.Context) ([]Message, error) {
	url := fmt.Sprintf("%s/hubs/%s/messages", c.baseURL, c.hubID)

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(body))
	}

	var result MessagesResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding response: %w", err)
	}

	return result.Messages, nil
}

// AcknowledgeMessage marks a message as processed.
func (c *Client) AcknowledgeMessage(ctx context.Context, messageID int64) error {
	url := fmt.Sprintf("%s/hubs/%s/messages/%d", c.baseURL, c.hubID, messageID)

	req, err := http.NewRequestWithContext(ctx, "PATCH", url, nil)
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(body))
	}

	return nil
}

// Heartbeat updates the hub's last_seen_at timestamp (simple heartbeat).
func (c *Client) Heartbeat(ctx context.Context) error {
	url := fmt.Sprintf("%s/hubs/%s/heartbeat", c.baseURL, c.hubID)

	req, err := http.NewRequestWithContext(ctx, "PATCH", url, nil)
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(body))
	}

	return nil
}

// AgentHeartbeatInfo contains agent information for heartbeat payloads.
type AgentHeartbeatInfo struct {
	SessionKey        string  `json:"session_key"`
	LastInvocationURL *string `json:"last_invocation_url,omitempty"`
}

// HeartbeatPayload is the request body for PUT /hubs/:id.
type HeartbeatPayload struct {
	Repo   string               `json:"repo"`
	Agents []AgentHeartbeatInfo `json:"agents"`
}

// SendHeartbeat registers the hub and its agents with the server.
// Uses RESTful PUT for upsert semantics.
func (c *Client) SendHeartbeat(ctx context.Context, repo string, agents []AgentHeartbeatInfo) (bool, error) {
	url := fmt.Sprintf("%s/hubs/%s", c.baseURL, c.hubID)

	payload := HeartbeatPayload{
		Repo:   repo,
		Agents: agents,
	}

	body, err := json.Marshal(payload)
	if err != nil {
		return false, fmt.Errorf("encoding payload: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "PUT", url, bytes.NewReader(body))
	if err != nil {
		return false, fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		c.logger.Warn("Failed to send heartbeat", "error", err)
		return false, nil
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		c.logger.Debug("Heartbeat sent successfully", "agents", len(agents))
		return true, nil
	}

	respBody, _ := io.ReadAll(resp.Body)
	c.logger.Warn("Heartbeat failed", "status", resp.StatusCode, "body", string(respBody))
	return false, nil
}

// NotificationPayload is the request body for POST /hubs/:id/notifications.
type NotificationPayload struct {
	Repo             string  `json:"repo"`
	IssueNumber      *int    `json:"issue_number,omitempty"`
	InvocationURL    *string `json:"invocation_url,omitempty"`
	NotificationType string  `json:"notification_type"`
}

// SendNotification sends an agent notification to trigger a GitHub comment.
func (c *Client) SendNotification(ctx context.Context, repo string, issueNumber *int, invocationURL *string, notificationType string) error {
	url := fmt.Sprintf("%s/hubs/%s/notifications", c.baseURL, c.hubID)

	payload := NotificationPayload{
		Repo:             repo,
		IssueNumber:      issueNumber,
		InvocationURL:    invocationURL,
		NotificationType: notificationType,
	}

	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("encoding payload: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		c.logger.Info("Sent notification to Rails",
			"repo", repo,
			"issue_number", issueNumber,
			"invocation_url", invocationURL,
			"type", notificationType,
		)
		return nil
	}

	respBody, _ := io.ReadAll(resp.Body)
	return fmt.Errorf("failed to send notification: %d - %s", resp.StatusCode, string(respBody))
}

// BrowserKeyResponse is the response from POST /hubs/:id/tailscale/browser_key.
type BrowserKeyResponse struct {
	Key   string `json:"key,omitempty"`
	Error string `json:"error,omitempty"`
}

// GetBrowserKey requests a browser pre-auth key for the QR code.
func (c *Client) GetBrowserKey(ctx context.Context) (string, error) {
	url := fmt.Sprintf("%s/hubs/%s/tailscale/browser_key", c.baseURL, c.hubID)

	req, err := http.NewRequestWithContext(ctx, "POST", url, nil)
	if err != nil {
		return "", fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	var result BrowserKeyResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return "", fmt.Errorf("decoding response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("failed to get browser key: %s", result.Error)
	}

	return result.Key, nil
}

// UpdateHostname updates the hub's Tailscale hostname.
func (c *Client) UpdateHostname(ctx context.Context, hostname string) error {
	url := fmt.Sprintf("%s/hubs/%s/tailscale/hostname", c.baseURL, c.hubID)

	body, err := json.Marshal(map[string]string{"hostname": hostname})
	if err != nil {
		return fmt.Errorf("encoding body: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "PATCH", url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)
	req.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		respBody, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(respBody))
	}

	return nil
}

// TailscaleStatus is the response from GET /hubs/:id/tailscale/status.
type TailscaleStatus struct {
	Connected         bool   `json:"connected"`
	Hostname          string `json:"hostname,omitempty"`
	PreauthKeyPresent bool   `json:"preauth_key_present"`
}

// GetTailscaleStatus gets the hub's Tailscale connection status.
func (c *Client) GetTailscaleStatus(ctx context.Context) (*TailscaleStatus, error) {
	url := fmt.Sprintf("%s/hubs/%s/tailscale/status", c.baseURL, c.hubID)

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	c.setAuthHeader(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(body))
	}

	var result TailscaleStatus
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding response: %w", err)
	}

	return &result, nil
}

func (c *Client) setAuthHeader(req *http.Request) {
	req.Header.Set("Authorization", "Bearer "+c.apiToken)
}

// DeviceCodeResponse is the response from POST /auth/device/code.
type DeviceCodeResponse struct {
	DeviceCode      string `json:"device_code"`
	UserCode        string `json:"user_code"`
	VerificationURL string `json:"verification_url"`
	ExpiresIn       int    `json:"expires_in"`
	Interval        int    `json:"interval"`
}

// DeviceTokenResponse is the response from POST /auth/device/token.
type DeviceTokenResponse struct {
	AccessToken string `json:"access_token,omitempty"`
	TokenType   string `json:"token_type,omitempty"`
	Error       string `json:"error,omitempty"`
}

// RequestDeviceCode initiates the device authorization flow.
// Returns a DeviceCodeResponse containing the user code and verification URL.
func RequestDeviceCode(ctx context.Context, baseURL string) (*DeviceCodeResponse, error) {
	url := baseURL + "/auth/device/code"

	req, err := http.NewRequestWithContext(ctx, "POST", url, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}

	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("unexpected status %d: %s", resp.StatusCode, string(body))
	}

	var result DeviceCodeResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding response: %w", err)
	}

	return &result, nil
}

// PollDeviceToken polls for the access token during device authorization.
// Returns the token when authorization is complete, or an error describing the state.
func PollDeviceToken(ctx context.Context, baseURL, deviceCode string) (*DeviceTokenResponse, error) {
	url := baseURL + "/auth/device/token"

	body, err := json.Marshal(map[string]string{"device_code": deviceCode})
	if err != nil {
		return nil, fmt.Errorf("encoding body: %w", err)
	}

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("making request: %w", err)
	}
	defer resp.Body.Close()

	var result DeviceTokenResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding response: %w", err)
	}

	return &result, nil
}
