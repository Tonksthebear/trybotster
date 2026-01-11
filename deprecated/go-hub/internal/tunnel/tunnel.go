// Package tunnel provides HTTP tunneling for agent dev servers.
//
// Uses WebSocket-based tunneling to forward HTTP requests from the Rails
// server to local dev servers running in agent worktrees. Supports multiple
// concurrent agent tunnels with automatic port allocation.
package tunnel

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

// Status represents tunnel connection state.
type Status int32

const (
	StatusDisconnected Status = iota
	StatusConnecting
	StatusConnected
)

func (s Status) String() string {
	switch s {
	case StatusConnecting:
		return "connecting"
	case StatusConnected:
		return "connected"
	default:
		return "disconnected"
	}
}

// PendingRegistration represents an agent waiting to be registered with Rails.
type PendingRegistration struct {
	SessionKey string
	Port       uint16
}

// Manager manages tunnel connections for all agents on a hub.
type Manager struct {
	hubIdentifier string
	apiKey        string
	serverURL     string

	// Map of session_key -> allocated port
	agentPorts   map[string]uint16
	agentPortsMu sync.RWMutex

	// Connection status (atomic for lock-free TUI access)
	status atomic.Int32

	// Channel for pending agent registrations
	pendingCh chan PendingRegistration

	// HTTP client for forwarding requests
	httpClient *http.Client
}

// NewManager creates a new tunnel manager.
func NewManager(hubIdentifier, apiKey, serverURL string) *Manager {
	return &Manager{
		hubIdentifier: hubIdentifier,
		apiKey:        apiKey,
		serverURL:     serverURL,
		agentPorts:    make(map[string]uint16),
		pendingCh:     make(chan PendingRegistration, 100),
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
			CheckRedirect: func(req *http.Request, via []*http.Request) error {
				// Don't follow redirects - return them to browser for OAuth flows
				return http.ErrUseLastResponse
			},
		},
	}
}

// GetStatus returns the current tunnel connection status.
func (m *Manager) GetStatus() Status {
	return Status(m.status.Load())
}

func (m *Manager) setStatus(s Status) {
	m.status.Store(int32(s))
}

// RegisterAgent registers an agent's tunnel port and queues notification to Rails.
func (m *Manager) RegisterAgent(sessionKey string, port uint16) {
	m.agentPortsMu.Lock()
	m.agentPorts[sessionKey] = port
	m.agentPortsMu.Unlock()

	// Queue notification to Rails (non-blocking)
	select {
	case m.pendingCh <- PendingRegistration{SessionKey: sessionKey, Port: port}:
	default:
		// Channel full, registration will be sent on reconnect
	}
}

// UnregisterAgent removes an agent's tunnel registration.
func (m *Manager) UnregisterAgent(sessionKey string) {
	m.agentPortsMu.Lock()
	delete(m.agentPorts, sessionKey)
	m.agentPortsMu.Unlock()
}

// GetAgentPort returns the port for an agent.
func (m *Manager) GetAgentPort(sessionKey string) (uint16, bool) {
	m.agentPortsMu.RLock()
	port, ok := m.agentPorts[sessionKey]
	m.agentPortsMu.RUnlock()
	return port, ok
}

// Connect establishes a WebSocket connection and starts the message loop.
func (m *Manager) Connect(ctx context.Context) error {
	wsURL := strings.Replace(m.serverURL, "https://", "wss://", 1)
	wsURL = strings.Replace(wsURL, "http://", "ws://", 1)
	wsURL += "/cable"

	m.setStatus(StatusConnecting)

	// Build headers
	header := http.Header{}
	header.Set("Origin", m.serverURL)
	header.Set("Authorization", fmt.Sprintf("Bearer %s", m.apiKey))

	dialer := websocket.Dialer{
		HandshakeTimeout: 10 * time.Second,
	}

	conn, _, err := dialer.DialContext(ctx, wsURL, header)
	if err != nil {
		m.setStatus(StatusDisconnected)
		return fmt.Errorf("websocket connect failed: %w", err)
	}
	defer conn.Close()

	// Subscribe to tunnel channel
	if err := m.subscribe(conn); err != nil {
		m.setStatus(StatusDisconnected)
		return fmt.Errorf("subscribe failed: %w", err)
	}

	// Message loop
	return m.messageLoop(ctx, conn)
}

func (m *Manager) subscribe(conn *websocket.Conn) error {
	identifier, _ := json.Marshal(map[string]string{
		"channel": "TunnelChannel",
		"hub_id":  m.hubIdentifier,
	})

	msg := map[string]string{
		"command":    "subscribe",
		"identifier": string(identifier),
	}

	return conn.WriteJSON(msg)
}

func (m *Manager) messageLoop(ctx context.Context, conn *websocket.Conn) error {
	// Start reader goroutine
	msgCh := make(chan []byte, 10)
	errCh := make(chan error, 1)

	go func() {
		for {
			_, data, err := conn.ReadMessage()
			if err != nil {
				errCh <- err
				return
			}
			msgCh <- data
		}
	}()

	for {
		select {
		case <-ctx.Done():
			m.setStatus(StatusDisconnected)
			return ctx.Err()

		case err := <-errCh:
			m.setStatus(StatusDisconnected)
			return fmt.Errorf("websocket read error: %w", err)

		case data := <-msgCh:
			if err := m.handleMessage(conn, data); err != nil {
				// Log but don't disconnect on message handling errors
				continue
			}

		case reg := <-m.pendingCh:
			// Only send if connected
			if m.GetStatus() == StatusConnected {
				_ = m.notifyAgentTunnel(conn, reg.SessionKey, reg.Port)
			} else {
				// Re-queue for later (non-blocking)
				select {
				case m.pendingCh <- reg:
				default:
				}
			}
		}
	}
}

func (m *Manager) handleMessage(conn *websocket.Conn, data []byte) error {
	var msg map[string]interface{}
	if err := json.Unmarshal(data, &msg); err != nil {
		return err
	}

	// Handle ActionCable protocol messages
	if msgType, ok := msg["type"].(string); ok {
		switch msgType {
		case "welcome":
			// ActionCable welcome received
		case "confirm_subscription":
			m.setStatus(StatusConnected)
			// Send all existing registered agents
			m.agentPortsMu.RLock()
			ports := make(map[string]uint16, len(m.agentPorts))
			for k, v := range m.agentPorts {
				ports[k] = v
			}
			m.agentPortsMu.RUnlock()

			for sessionKey, port := range ports {
				_ = m.notifyAgentTunnel(conn, sessionKey, port)
			}
		case "reject_subscription":
			// Hub doesn't exist yet - will retry after heartbeat creates it
		case "disconnect":
			m.setStatus(StatusDisconnected)
		case "ping":
			// ActionCable ping, no response needed
		}
		return nil
	}

	// Handle actual messages
	message, ok := msg["message"].(map[string]interface{})
	if !ok {
		return nil
	}

	msgType, _ := message["type"].(string)
	if msgType != "http_request" {
		return nil
	}

	requestID, _ := message["request_id"].(string)
	agentSessionKey, _ := message["agent_session_key"].(string)

	port, ok := m.GetAgentPort(agentSessionKey)
	if !ok {
		return m.sendErrorResponse(conn, requestID, "Agent tunnel not registered")
	}

	method, _ := message["method"].(string)
	if method == "" {
		method = "GET"
	}
	path, _ := message["path"].(string)
	if path == "" {
		path = "/"
	}
	query, _ := message["query_string"].(string)
	headers := extractHeaders(message["headers"])
	body, _ := message["body"].(string)

	// Forward to local server
	resp := m.forwardRequest(port, method, path, query, headers, body)

	// Send response back via ActionCable
	return m.sendHTTPResponse(conn, requestID, resp)
}

func extractHeaders(v interface{}) map[string]string {
	result := make(map[string]string)
	if m, ok := v.(map[string]interface{}); ok {
		for k, val := range m {
			if s, ok := val.(string); ok {
				result[k] = s
			}
		}
	}
	return result
}

// TunnelResponse represents an HTTP response to forward back.
type TunnelResponse struct {
	Status      int
	Headers     map[string]string
	Body        string
	ContentType string
}

func (m *Manager) forwardRequest(port uint16, method, path, query string, headers map[string]string, body string) TunnelResponse {
	url := fmt.Sprintf("http://localhost:%d%s", port, path)
	if query != "" {
		url += "?" + query
	}

	var bodyReader io.Reader
	if body != "" && method != "GET" && method != "HEAD" {
		bodyReader = strings.NewReader(body)
	}

	req, err := http.NewRequest(method, url, bodyReader)
	if err != nil {
		return TunnelResponse{
			Status:      502,
			Headers:     map[string]string{},
			Body:        fmt.Sprintf("Failed to create request: %v", err),
			ContentType: "text/plain",
		}
	}

	for k, v := range headers {
		req.Header.Set(k, v)
	}

	resp, err := m.httpClient.Do(req)
	if err != nil {
		return TunnelResponse{
			Status:      502,
			Headers:     map[string]string{},
			Body:        fmt.Sprintf("Failed to connect to local server on port %d: %v", port, err),
			ContentType: "text/plain",
		}
	}
	defer resp.Body.Close()

	respBody, _ := io.ReadAll(resp.Body)

	contentType := resp.Header.Get("Content-Type")
	if contentType == "" {
		contentType = "text/html"
	}

	// Filter out headers that shouldn't be forwarded
	respHeaders := make(map[string]string)
	for k, v := range resp.Header {
		lower := strings.ToLower(k)
		if lower != "content-encoding" && lower != "transfer-encoding" {
			if len(v) > 0 {
				respHeaders[k] = v[0]
			}
		}
	}

	return TunnelResponse{
		Status:      resp.StatusCode,
		Headers:     respHeaders,
		Body:        string(respBody),
		ContentType: contentType,
	}
}

func (m *Manager) sendHTTPResponse(conn *websocket.Conn, requestID string, resp TunnelResponse) error {
	identifier, _ := json.Marshal(map[string]string{
		"channel": "TunnelChannel",
		"hub_id":  m.hubIdentifier,
	})

	data, _ := json.Marshal(map[string]interface{}{
		"action":       "http_response",
		"request_id":   requestID,
		"status":       resp.Status,
		"headers":      resp.Headers,
		"body":         resp.Body,
		"content_type": resp.ContentType,
	})

	msg := map[string]string{
		"command":    "message",
		"identifier": string(identifier),
		"data":       string(data),
	}

	return conn.WriteJSON(msg)
}

func (m *Manager) sendErrorResponse(conn *websocket.Conn, requestID, errMsg string) error {
	return m.sendHTTPResponse(conn, requestID, TunnelResponse{
		Status:      502,
		Headers:     map[string]string{},
		Body:        errMsg,
		ContentType: "text/plain",
	})
}

func (m *Manager) notifyAgentTunnel(conn *websocket.Conn, sessionKey string, port uint16) error {
	identifier, _ := json.Marshal(map[string]string{
		"channel": "TunnelChannel",
		"hub_id":  m.hubIdentifier,
	})

	data, _ := json.Marshal(map[string]interface{}{
		"action":      "register_agent_tunnel",
		"session_key": sessionKey,
		"port":        port,
	})

	msg := map[string]string{
		"command":    "message",
		"identifier": string(identifier),
		"data":       string(data),
	}

	return conn.WriteJSON(msg)
}

// AllocateTunnelPort finds an available port in the 4001-4999 range.
func AllocateTunnelPort() (uint16, error) {
	for port := uint16(4001); port < 5000; port++ {
		listener, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
		if err == nil {
			listener.Close()
			return port, nil
		}
	}
	return 0, fmt.Errorf("no available ports in range 4001-4999")
}
