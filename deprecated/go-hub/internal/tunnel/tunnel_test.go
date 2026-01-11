package tunnel

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func TestNewManager(t *testing.T) {
	m := NewManager("hub123", "apikey", "https://example.com")

	if m.hubIdentifier != "hub123" {
		t.Errorf("hubIdentifier = %q, want 'hub123'", m.hubIdentifier)
	}
	if m.apiKey != "apikey" {
		t.Errorf("apiKey = %q", m.apiKey)
	}
	if m.serverURL != "https://example.com" {
		t.Errorf("serverURL = %q", m.serverURL)
	}
	if m.GetStatus() != StatusDisconnected {
		t.Errorf("initial status = %v, want StatusDisconnected", m.GetStatus())
	}
}

func TestStatusString(t *testing.T) {
	tests := []struct {
		status Status
		want   string
	}{
		{StatusDisconnected, "disconnected"},
		{StatusConnecting, "connecting"},
		{StatusConnected, "connected"},
	}

	for _, tt := range tests {
		if got := tt.status.String(); got != tt.want {
			t.Errorf("Status(%d).String() = %q, want %q", tt.status, got, tt.want)
		}
	}
}

func TestGetSetStatus(t *testing.T) {
	m := NewManager("hub", "key", "https://example.com")

	if m.GetStatus() != StatusDisconnected {
		t.Error("initial status should be disconnected")
	}

	m.setStatus(StatusConnecting)
	if m.GetStatus() != StatusConnecting {
		t.Errorf("status = %v, want StatusConnecting", m.GetStatus())
	}

	m.setStatus(StatusConnected)
	if m.GetStatus() != StatusConnected {
		t.Errorf("status = %v, want StatusConnected", m.GetStatus())
	}

	m.setStatus(StatusDisconnected)
	if m.GetStatus() != StatusDisconnected {
		t.Errorf("status = %v, want StatusDisconnected", m.GetStatus())
	}
}

func TestRegisterUnregisterAgent(t *testing.T) {
	m := NewManager("hub", "key", "https://example.com")

	// Register agent
	m.RegisterAgent("agent-1", 4001)

	port, ok := m.GetAgentPort("agent-1")
	if !ok {
		t.Fatal("agent should be registered")
	}
	if port != 4001 {
		t.Errorf("port = %d, want 4001", port)
	}

	// Unknown agent
	_, ok = m.GetAgentPort("unknown")
	if ok {
		t.Error("unknown agent should not be found")
	}

	// Unregister
	m.UnregisterAgent("agent-1")
	_, ok = m.GetAgentPort("agent-1")
	if ok {
		t.Error("agent should be unregistered")
	}
}

func TestRegisterMultipleAgents(t *testing.T) {
	m := NewManager("hub", "key", "https://example.com")

	m.RegisterAgent("agent-1", 4001)
	m.RegisterAgent("agent-2", 4002)
	m.RegisterAgent("agent-3", 4003)

	port1, ok1 := m.GetAgentPort("agent-1")
	port2, ok2 := m.GetAgentPort("agent-2")
	port3, ok3 := m.GetAgentPort("agent-3")

	if !ok1 || port1 != 4001 {
		t.Errorf("agent-1: port=%d, ok=%v", port1, ok1)
	}
	if !ok2 || port2 != 4002 {
		t.Errorf("agent-2: port=%d, ok=%v", port2, ok2)
	}
	if !ok3 || port3 != 4003 {
		t.Errorf("agent-3: port=%d, ok=%v", port3, ok3)
	}
}

func TestAllocateTunnelPort(t *testing.T) {
	port, err := AllocateTunnelPort()
	if err != nil {
		t.Fatalf("AllocateTunnelPort() error = %v", err)
	}
	if port < 4001 || port > 4999 {
		t.Errorf("port = %d, want in range 4001-4999", port)
	}
}

func TestExtractHeaders(t *testing.T) {
	input := map[string]interface{}{
		"Content-Type":   "application/json",
		"X-Custom":       "value",
		"Invalid":        123, // Non-string ignored
		"Accept-Charset": nil, // Nil ignored
	}

	result := extractHeaders(input)

	if result["Content-Type"] != "application/json" {
		t.Errorf("Content-Type = %q", result["Content-Type"])
	}
	if result["X-Custom"] != "value" {
		t.Errorf("X-Custom = %q", result["X-Custom"])
	}
	if _, ok := result["Invalid"]; ok {
		t.Error("Invalid should not be in result")
	}
}

func TestExtractHeadersNil(t *testing.T) {
	result := extractHeaders(nil)
	if len(result) != 0 {
		t.Errorf("extractHeaders(nil) should return empty map, got %v", result)
	}
}

func TestForwardRequest(t *testing.T) {
	// Start a test server
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" {
			t.Errorf("method = %s, want POST", r.Method)
		}
		if r.URL.Path != "/test" {
			t.Errorf("path = %s, want /test", r.URL.Path)
		}
		if r.URL.RawQuery != "foo=bar" {
			t.Errorf("query = %s, want foo=bar", r.URL.RawQuery)
		}
		if r.Header.Get("X-Custom") != "header" {
			t.Errorf("X-Custom header = %s", r.Header.Get("X-Custom"))
		}

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(200)
		w.Write([]byte(`{"result":"ok"}`))
	}))
	defer server.Close()

	// Extract port from test server URL
	parts := strings.Split(server.URL, ":")
	port := parts[len(parts)-1]

	m := NewManager("hub", "key", "https://example.com")

	// Parse port as uint16
	var portNum uint16
	for _, c := range port {
		portNum = portNum*10 + uint16(c-'0')
	}

	resp := m.forwardRequest(
		portNum,
		"POST",
		"/test",
		"foo=bar",
		map[string]string{"X-Custom": "header"},
		`{"data":"test"}`,
	)

	if resp.Status != 200 {
		t.Errorf("Status = %d, want 200", resp.Status)
	}
	if resp.ContentType != "application/json" {
		t.Errorf("ContentType = %s", resp.ContentType)
	}
	if resp.Body != `{"result":"ok"}` {
		t.Errorf("Body = %s", resp.Body)
	}
}

func TestForwardRequestConnectionError(t *testing.T) {
	m := NewManager("hub", "key", "https://example.com")

	// Use a port that's definitely not listening
	resp := m.forwardRequest(59999, "GET", "/", "", nil, "")

	if resp.Status != 502 {
		t.Errorf("Status = %d, want 502", resp.Status)
	}
	if !strings.Contains(resp.Body, "Failed to connect") {
		t.Errorf("Body should contain error message: %s", resp.Body)
	}
}

func TestForwardRequestRedirectNotFollowed(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, "https://external.com/oauth", http.StatusFound)
	}))
	defer server.Close()

	parts := strings.Split(server.URL, ":")
	port := parts[len(parts)-1]
	var portNum uint16
	for _, c := range port {
		portNum = portNum*10 + uint16(c-'0')
	}

	m := NewManager("hub", "key", "https://example.com")
	resp := m.forwardRequest(portNum, "GET", "/login", "", nil, "")

	// Should return the redirect, not follow it
	if resp.Status != 302 {
		t.Errorf("Status = %d, want 302", resp.Status)
	}
	if resp.Headers["Location"] != "https://external.com/oauth" {
		t.Errorf("Location header = %s", resp.Headers["Location"])
	}
}

func TestTunnelResponse(t *testing.T) {
	resp := TunnelResponse{
		Status:      200,
		Headers:     map[string]string{"X-Custom": "value"},
		Body:        "response body",
		ContentType: "text/plain",
	}

	if resp.Status != 200 {
		t.Errorf("Status = %d", resp.Status)
	}
	if resp.Headers["X-Custom"] != "value" {
		t.Errorf("Headers = %v", resp.Headers)
	}
	if resp.Body != "response body" {
		t.Errorf("Body = %s", resp.Body)
	}
	if resp.ContentType != "text/plain" {
		t.Errorf("ContentType = %s", resp.ContentType)
	}
}

func TestPendingRegistrationStruct(t *testing.T) {
	reg := PendingRegistration{
		SessionKey: "agent-1",
		Port:       4001,
	}

	if reg.SessionKey != "agent-1" {
		t.Errorf("SessionKey = %s", reg.SessionKey)
	}
	if reg.Port != 4001 {
		t.Errorf("Port = %d", reg.Port)
	}
}

// WebSocket integration tests

var upgrader = websocket.Upgrader{
	CheckOrigin: func(r *http.Request) bool { return true },
}

func TestConnectAndSubscribe(t *testing.T) {
	// Create WebSocket test server
	subscribed := make(chan bool, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			t.Logf("upgrade error: %v", err)
			return
		}
		defer conn.Close()

		// Send welcome
		conn.WriteJSON(map[string]string{"type": "welcome"})

		// Read subscribe message
		var msg map[string]string
		if err := conn.ReadJSON(&msg); err != nil {
			t.Logf("read error: %v", err)
			return
		}

		if msg["command"] != "subscribe" {
			t.Errorf("command = %s, want subscribe", msg["command"])
		}

		var identifier map[string]string
		json.Unmarshal([]byte(msg["identifier"]), &identifier)
		if identifier["channel"] != "TunnelChannel" {
			t.Errorf("channel = %s", identifier["channel"])
		}
		if identifier["hub_id"] != "test-hub" {
			t.Errorf("hub_id = %s", identifier["hub_id"])
		}

		// Send confirm
		conn.WriteJSON(map[string]string{"type": "confirm_subscription"})
		subscribed <- true

		// Keep connection alive briefly
		time.Sleep(100 * time.Millisecond)
	}))
	defer server.Close()

	m := NewManager("test-hub", "apikey", server.URL)

	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()

	go func() {
		m.Connect(ctx)
	}()

	select {
	case <-subscribed:
		// Wait for status to update
		time.Sleep(50 * time.Millisecond)
		if m.GetStatus() != StatusConnected {
			t.Errorf("status = %v, want StatusConnected", m.GetStatus())
		}
	case <-time.After(500 * time.Millisecond):
		t.Error("timeout waiting for subscription")
	}
}

func TestHandleHTTPRequest(t *testing.T) {
	// Start a local "dev server"
	devServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"dev":"response"}`))
	}))
	defer devServer.Close()

	parts := strings.Split(devServer.URL, ":")
	port := parts[len(parts)-1]
	var portNum uint16
	for _, c := range port {
		portNum = portNum*10 + uint16(c-'0')
	}

	// Test handleMessage directly instead of full websocket flow
	m := NewManager("test-hub", "apikey", "https://example.com")
	m.RegisterAgent("agent-1", portNum)
	m.setStatus(StatusConnected)

	// Create a mock websocket connection
	serverDone := make(chan struct{})
	wsServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		defer close(serverDone)

		// Read and verify the response message
		var resp map[string]interface{}
		conn.SetReadDeadline(time.Now().Add(2 * time.Second))
		if err := conn.ReadJSON(&resp); err != nil {
			t.Errorf("failed to read response: %v", err)
			return
		}

		data, ok := resp["data"].(string)
		if !ok {
			t.Errorf("data is not string: %T", resp["data"])
			return
		}

		var dataMap map[string]interface{}
		if err := json.Unmarshal([]byte(data), &dataMap); err != nil {
			t.Errorf("failed to parse data: %v", err)
			return
		}

		if dataMap["action"] != "http_response" {
			t.Errorf("action = %v, want http_response", dataMap["action"])
		}
		if dataMap["request_id"] != "req-123" {
			t.Errorf("request_id = %v", dataMap["request_id"])
		}
		status, _ := dataMap["status"].(float64)
		if int(status) != 200 {
			t.Errorf("status = %v, want 200", dataMap["status"])
		}
		if dataMap["body"] != `{"dev":"response"}` {
			t.Errorf("body = %v", dataMap["body"])
		}
	}))
	defer wsServer.Close()

	// Connect to the mock server
	wsURL := strings.Replace(wsServer.URL, "http://", "ws://", 1)
	conn, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("failed to connect: %v", err)
	}
	defer conn.Close()

	// Simulate receiving an http_request message
	httpRequestMsg, _ := json.Marshal(map[string]interface{}{
		"message": map[string]interface{}{
			"type":               "http_request",
			"request_id":        "req-123",
			"agent_session_key": "agent-1",
			"method":            "GET",
			"path":              "/api",
			"query_string":      "",
			"headers":           map[string]string{},
			"body":              "",
		},
	})

	// Call handleMessage directly
	if err := m.handleMessage(conn, httpRequestMsg); err != nil {
		t.Errorf("handleMessage error: %v", err)
	}

	select {
	case <-serverDone:
		// Server received and validated response
	case <-time.After(3 * time.Second):
		t.Error("timeout waiting for server to receive response")
	}
}

func TestHandleDisconnect(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		conn.WriteJSON(map[string]string{"type": "welcome"})
		conn.ReadJSON(&map[string]string{})
		conn.WriteJSON(map[string]string{"type": "confirm_subscription"})

		time.Sleep(50 * time.Millisecond)
		conn.WriteJSON(map[string]string{"type": "disconnect"})

		time.Sleep(100 * time.Millisecond)
	}))
	defer server.Close()

	m := NewManager("test-hub", "apikey", server.URL)

	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()

	go func() {
		m.Connect(ctx)
	}()

	time.Sleep(150 * time.Millisecond)
	if m.GetStatus() != StatusDisconnected {
		t.Errorf("status after disconnect = %v, want StatusDisconnected", m.GetStatus())
	}
}

func TestHandleRejectSubscription(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		conn.WriteJSON(map[string]string{"type": "welcome"})
		conn.ReadJSON(&map[string]string{})
		conn.WriteJSON(map[string]string{"type": "reject_subscription"})

		time.Sleep(100 * time.Millisecond)
	}))
	defer server.Close()

	m := NewManager("test-hub", "apikey", server.URL)

	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()

	go func() {
		m.Connect(ctx)
	}()

	time.Sleep(150 * time.Millisecond)
	// Should still be connecting or disconnected, not connected
	if m.GetStatus() == StatusConnected {
		t.Error("status should not be connected after reject_subscription")
	}
}

func TestConnectFailure(t *testing.T) {
	m := NewManager("test-hub", "apikey", "http://localhost:59999")

	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()

	err := m.Connect(ctx)
	if err == nil {
		t.Error("expected error for failed connection")
	}
	if m.GetStatus() != StatusDisconnected {
		t.Errorf("status = %v, want StatusDisconnected", m.GetStatus())
	}
}

func TestConcurrentAgentRegistration(t *testing.T) {
	m := NewManager("hub", "key", "https://example.com")

	// Register many agents concurrently
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			key := string(rune('a'+n%26)) + string(rune('0'+n%10))
			m.RegisterAgent(key, uint16(4000+n))
		}(i)
	}
	wg.Wait()

	// Verify some registrations
	if port, ok := m.GetAgentPort("a0"); !ok || port != 4000 {
		t.Errorf("a0: port=%d, ok=%v", port, ok)
	}
}

func TestSendExistingAgentsOnConnect(t *testing.T) {
	registrationCount := 0
	var mu sync.Mutex

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		conn.WriteJSON(map[string]string{"type": "welcome"})
		conn.ReadJSON(&map[string]string{}) // subscribe
		conn.WriteJSON(map[string]string{"type": "confirm_subscription"})

		// Read registration messages
		for i := 0; i < 3; i++ {
			var msg map[string]string
			conn.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
			if err := conn.ReadJSON(&msg); err != nil {
				break
			}
			if msg["command"] == "message" {
				mu.Lock()
				registrationCount++
				mu.Unlock()
			}
		}

		time.Sleep(100 * time.Millisecond)
	}))
	defer server.Close()

	m := NewManager("test-hub", "apikey", server.URL)

	// Register agents BEFORE connecting
	m.RegisterAgent("agent-1", 4001)
	m.RegisterAgent("agent-2", 4002)
	m.RegisterAgent("agent-3", 4003)

	ctx, cancel := context.WithTimeout(context.Background(), 1*time.Second)
	defer cancel()

	go func() {
		m.Connect(ctx)
	}()

	time.Sleep(600 * time.Millisecond)

	mu.Lock()
	count := registrationCount
	mu.Unlock()

	if count < 3 {
		t.Errorf("registrationCount = %d, want at least 3", count)
	}
}
