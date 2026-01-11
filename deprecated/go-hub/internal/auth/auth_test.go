package auth

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestDeviceCodeResponseDeserialize(t *testing.T) {
	jsonData := `{
		"device_code": "abc123",
		"user_code": "WDJB-MJHT",
		"verification_uri": "https://example.com/device",
		"expires_in": 900,
		"interval": 5
	}`

	var resp DeviceCodeResponse
	if err := json.Unmarshal([]byte(jsonData), &resp); err != nil {
		t.Fatalf("Unmarshal failed: %v", err)
	}

	if resp.DeviceCode != "abc123" {
		t.Errorf("DeviceCode = %q, want %q", resp.DeviceCode, "abc123")
	}
	if resp.UserCode != "WDJB-MJHT" {
		t.Errorf("UserCode = %q, want %q", resp.UserCode, "WDJB-MJHT")
	}
	if resp.VerificationURI != "https://example.com/device" {
		t.Errorf("VerificationURI = %q, want %q", resp.VerificationURI, "https://example.com/device")
	}
	if resp.ExpiresIn != 900 {
		t.Errorf("ExpiresIn = %d, want %d", resp.ExpiresIn, 900)
	}
	if resp.Interval != 5 {
		t.Errorf("Interval = %d, want %d", resp.Interval, 5)
	}
}

func TestTokenResponseDeserialize(t *testing.T) {
	jsonData := `{
		"access_token": "btstr_xyz789",
		"token_type": "bearer"
	}`

	var resp TokenResponse
	if err := json.Unmarshal([]byte(jsonData), &resp); err != nil {
		t.Fatalf("Unmarshal failed: %v", err)
	}

	if resp.AccessToken != "btstr_xyz789" {
		t.Errorf("AccessToken = %q, want %q", resp.AccessToken, "btstr_xyz789")
	}
	if resp.TokenType != "bearer" {
		t.Errorf("TokenType = %q, want %q", resp.TokenType, "bearer")
	}
}

func TestErrorResponseDeserialize(t *testing.T) {
	tests := []struct {
		name     string
		jsonData string
		want     string
	}{
		{
			name:     "authorization_pending",
			jsonData: `{"error": "authorization_pending"}`,
			want:     "authorization_pending",
		},
		{
			name:     "expired_token",
			jsonData: `{"error": "expired_token"}`,
			want:     "expired_token",
		},
		{
			name:     "access_denied",
			jsonData: `{"error": "access_denied"}`,
			want:     "access_denied",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var resp ErrorResponse
			if err := json.Unmarshal([]byte(tt.jsonData), &resp); err != nil {
				t.Fatalf("Unmarshal failed: %v", err)
			}
			if resp.Error != tt.want {
				t.Errorf("Error = %q, want %q", resp.Error, tt.want)
			}
		})
	}
}

func TestValidateTokenSuccess(t *testing.T) {
	// Mock server that returns 200 for valid token
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/devices" {
			t.Errorf("Unexpected path: %s", r.URL.Path)
		}

		auth := r.Header.Get("Authorization")
		if auth != "Bearer btstr_test123" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}

		w.WriteHeader(http.StatusOK)
		w.Write([]byte(`[]`))
	}))
	defer server.Close()

	if !ValidateToken(server.URL, "btstr_test123") {
		t.Error("ValidateToken returned false, want true")
	}
}

func TestValidateTokenInvalid(t *testing.T) {
	// Mock server that returns 401 for invalid token
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer server.Close()

	if ValidateToken(server.URL, "invalid_token") {
		t.Error("ValidateToken returned true, want false")
	}
}

func TestValidateTokenEmpty(t *testing.T) {
	if ValidateToken("http://example.com", "") {
		t.Error("ValidateToken returned true for empty token, want false")
	}
}

func TestDeviceFlowRequestsDeviceCode(t *testing.T) {
	// Track what was requested
	var gotDeviceName string

	// Mock server
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/hubs/codes" && r.Method == "POST" {
			var body map[string]string
			json.NewDecoder(r.Body).Decode(&body)
			gotDeviceName = body["device_name"]

			// Return device code response
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(DeviceCodeResponse{
				DeviceCode:      "test_device_code",
				UserCode:        "TEST-CODE",
				VerificationURI: "https://example.com/device",
				ExpiresIn:       5, // Short for test
				Interval:        1,
			})
			return
		}

		if r.URL.Path == "/hubs/codes/test_device_code" && r.Method == "GET" {
			// Return success immediately
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(TokenResponse{
				AccessToken: "btstr_test_token",
				TokenType:   "bearer",
			})
			return
		}

		w.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()

	// Run device flow - will timeout waiting for stdin but that's ok
	// This test just verifies the request is made correctly
	token, err := DeviceFlow(server.URL)
	if err != nil {
		t.Fatalf("DeviceFlow failed: %v", err)
	}

	if token != "btstr_test_token" {
		t.Errorf("Token = %q, want %q", token, "btstr_test_token")
	}

	if gotDeviceName == "" {
		t.Error("device_name was not sent in request")
	}
}

func TestDeviceFlowHandlesExpiredToken(t *testing.T) {
	pollCount := 0

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/hubs/codes" && r.Method == "POST" {
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(DeviceCodeResponse{
				DeviceCode:      "test_device_code",
				UserCode:        "TEST-CODE",
				VerificationURI: "https://example.com/device",
				ExpiresIn:       10,
				Interval:        1,
			})
			return
		}

		if r.URL.Path == "/hubs/codes/test_device_code" && r.Method == "GET" {
			pollCount++
			// Return expired after first poll
			w.WriteHeader(http.StatusBadRequest)
			json.NewEncoder(w).Encode(ErrorResponse{Error: "expired_token"})
			return
		}

		w.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()

	_, err := DeviceFlow(server.URL)
	if err == nil {
		t.Error("DeviceFlow should have failed with expired_token")
	}

	if pollCount == 0 {
		t.Error("Server was never polled")
	}
}

func TestDeviceFlowHandlesAccessDenied(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/hubs/codes" && r.Method == "POST" {
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(DeviceCodeResponse{
				DeviceCode:      "test_device_code",
				UserCode:        "TEST-CODE",
				VerificationURI: "https://example.com/device",
				ExpiresIn:       10,
				Interval:        1,
			})
			return
		}

		if r.URL.Path == "/hubs/codes/test_device_code" && r.Method == "GET" {
			w.WriteHeader(http.StatusForbidden)
			json.NewEncoder(w).Encode(ErrorResponse{Error: "access_denied"})
			return
		}

		w.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()

	_, err := DeviceFlow(server.URL)
	if err == nil {
		t.Error("DeviceFlow should have failed with access_denied")
	}
}

func TestDeviceFlowPollsPending(t *testing.T) {
	pollCount := 0

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/hubs/codes" && r.Method == "POST" {
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(DeviceCodeResponse{
				DeviceCode:      "test_device_code",
				UserCode:        "TEST-CODE",
				VerificationURI: "https://example.com/device",
				ExpiresIn:       20, // Longer expiry to allow multiple polls
				Interval:        5,
			})
			return
		}

		if r.URL.Path == "/hubs/codes/test_device_code" && r.Method == "GET" {
			pollCount++
			if pollCount < 2 {
				// Pending for first poll only
				w.WriteHeader(http.StatusAccepted)
				return
			}
			// Success on 2nd poll
			w.WriteHeader(http.StatusOK)
			json.NewEncoder(w).Encode(TokenResponse{
				AccessToken: "btstr_final_token",
				TokenType:   "bearer",
			})
			return
		}

		w.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()

	token, err := DeviceFlow(server.URL)
	if err != nil {
		t.Fatalf("DeviceFlow failed: %v", err)
	}

	if token != "btstr_final_token" {
		t.Errorf("Token = %q, want %q", token, "btstr_final_token")
	}

	if pollCount != 2 {
		t.Errorf("pollCount = %d, want 2", pollCount)
	}
}
