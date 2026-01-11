// Package auth implements RFC 8628 (OAuth 2.0 Device Authorization Grant)
// for CLI authentication without requiring manual API key configuration.
package auth

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"runtime"
	"time"

	"golang.org/x/term"
)

// DeviceCodeResponse is returned from POST /hubs/codes.
type DeviceCodeResponse struct {
	// DeviceCode is the opaque code for polling.
	DeviceCode string `json:"device_code"`
	// UserCode is the human-readable code to display to user.
	UserCode string `json:"user_code"`
	// VerificationURI is where the user should enter the code.
	VerificationURI string `json:"verification_uri"`
	// ExpiresIn is seconds until the code expires.
	ExpiresIn uint64 `json:"expires_in"`
	// Interval is the minimum polling interval in seconds.
	Interval uint64 `json:"interval"`
}

// TokenResponse is the successful response from GET /hubs/codes/:id.
type TokenResponse struct {
	// AccessToken is the API authentication token.
	AccessToken string `json:"access_token"`
	// TokenType is typically "Bearer".
	TokenType string `json:"token_type"`
}

// ErrorResponse is returned during polling on error.
type ErrorResponse struct {
	// Error code (e.g., "authorization_pending", "slow_down").
	Error string `json:"error"`
}

// DeviceFlow performs the device authorization flow to obtain an access token.
//
// This function will:
// 1. Request a device code from the server
// 2. Display the verification URL and user code to the user
// 3. Optionally open the browser (unless BOTSTER_NO_BROWSER is set)
// 4. Poll the server until the user approves or the code expires
// 5. Return the access token on success
func DeviceFlow(serverURL string) (string, error) {
	client := &http.Client{
		Timeout: 30 * time.Second,
	}

	// Get device name from hostname
	deviceName, err := os.Hostname()
	if err != nil || deviceName == "" {
		deviceName = "Botster CLI"
	}

	// Step 1: Request device code
	url := fmt.Sprintf("%s/hubs/codes", serverURL)
	body := map[string]string{"device_name": deviceName}
	jsonBody, err := json.Marshal(body)
	if err != nil {
		return "", fmt.Errorf("failed to marshal request: %w", err)
	}

	req, err := http.NewRequest("POST", url, bytes.NewReader(jsonBody))
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := client.Do(req)
	if err != nil {
		return "", fmt.Errorf("failed to request device code: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		respBody, _ := io.ReadAll(resp.Body)
		return "", fmt.Errorf("server returned %d: %s", resp.StatusCode, string(respBody))
	}

	var deviceCode DeviceCodeResponse
	if err := json.NewDecoder(resp.Body).Decode(&deviceCode); err != nil {
		return "", fmt.Errorf("invalid device code response: %w", err)
	}

	// Step 2: Display instructions to user
	fmt.Println()
	fmt.Println("  To authenticate, visit:")
	fmt.Println()
	fmt.Printf("    %s\n", deviceCode.VerificationURI)
	fmt.Println()
	fmt.Println("  And enter this code:")
	fmt.Println()
	fmt.Printf("    %s\n", deviceCode.UserCode)
	fmt.Println()

	// Check if we're in interactive mode (TTY)
	interactive := term.IsTerminal(int(os.Stdin.Fd())) &&
		os.Getenv("BOTSTER_NO_BROWSER") == "" &&
		os.Getenv("CI") == ""

	// Channel to signal browser thread completion
	browserDone := make(chan struct{})

	if interactive {
		fmt.Println("  Press Enter to open browser...")
		fmt.Println()

		// Spawn goroutine to listen for Enter key
		go func() {
			defer close(browserDone)
			reader := bufio.NewReader(os.Stdin)
			_, _ = reader.ReadString('\n')
			if err := openBrowser(deviceCode.VerificationURI); err != nil {
				fmt.Printf("\r  Could not open browser: %v         \n", err)
			} else {
				fmt.Print("\r  Browser opened.                    \n")
			}
		}()
	} else {
		fmt.Println("  Waiting for authorization...")
		fmt.Println()
		close(browserDone)
	}

	fmt.Print("  Polling")

	// Step 3: Poll for authorization
	pollURL := fmt.Sprintf("%s/hubs/codes/%s", serverURL, deviceCode.DeviceCode)
	pollInterval := deviceCode.Interval
	if pollInterval < 5 {
		pollInterval = 5
	}
	maxAttempts := deviceCode.ExpiresIn / pollInterval

	for attempt := uint64(0); attempt < maxAttempts; attempt++ {
		time.Sleep(time.Duration(pollInterval) * time.Second)

		resp, err := client.Get(pollURL)
		if err != nil {
			// Network error - retry
			fmt.Print(".")
			continue
		}

		status := resp.StatusCode

		switch status {
		case 200:
			// Success - we got the token
			var token TokenResponse
			if err := json.NewDecoder(resp.Body).Decode(&token); err != nil {
				resp.Body.Close()
				return "", fmt.Errorf("invalid token response: %w", err)
			}
			resp.Body.Close()

			fmt.Println()
			fmt.Println()
			fmt.Println("  Authorized successfully!")
			fmt.Println()
			return token.AccessToken, nil

		case 202:
			// Still pending - continue polling
			resp.Body.Close()
			fmt.Print(".")
			continue

		case 400, 401, 403:
			// Check error type
			var errResp ErrorResponse
			if err := json.NewDecoder(resp.Body).Decode(&errResp); err != nil {
				errResp.Error = "unknown"
			}
			resp.Body.Close()

			switch errResp.Error {
			case "authorization_pending":
				// Shouldn't happen with 400, but handle it
				fmt.Print(".")
				continue
			case "expired_token":
				fmt.Println()
				return "", fmt.Errorf("authorization code expired. Please try again")
			case "access_denied":
				fmt.Println()
				return "", fmt.Errorf("authorization was denied")
			default:
				fmt.Println()
				return "", fmt.Errorf("authorization failed: %s", errResp.Error)
			}

		default:
			resp.Body.Close()
			// Unexpected status - retry
			fmt.Print(".")
			continue
		}
	}

	fmt.Println()
	return "", fmt.Errorf("authorization timed out. Please try again")
}

// ValidateToken checks if a token is still valid by making a test API request.
// Returns true only if we get a successful response from an authenticated endpoint.
func ValidateToken(serverURL, token string) bool {
	if token == "" {
		fmt.Println("  Token validation: empty token")
		return false
	}

	client := &http.Client{
		Timeout: 10 * time.Second,
	}

	// Try to list devices - a simple authenticated endpoint
	url := fmt.Sprintf("%s/devices", serverURL)
	fmt.Printf("  Validating token against %s...\n", url)

	req, err := http.NewRequest("GET", url, nil)
	if err != nil {
		fmt.Printf("  Token validation: failed to create request: %v\n", err)
		return false
	}
	req.Header.Set("Authorization", "Bearer "+token)

	resp, err := client.Do(req)
	if err != nil {
		// Network error - could be server down, but we treat as "needs re-auth"
		fmt.Printf("  Token validation failed: %v\n", err)
		return false
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		fmt.Printf("  Token valid (status: %d)\n", resp.StatusCode)
		return true
	}

	fmt.Printf("  Token invalid (status: %d)\n", resp.StatusCode)
	return false
}

// openBrowser opens the given URL in the user's default browser.
func openBrowser(url string) error {
	var cmd *exec.Cmd

	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", url)
	case "linux":
		cmd = exec.Command("xdg-open", url)
	case "windows":
		cmd = exec.Command("cmd", "/C", "start", "", url)
	default:
		return fmt.Errorf("unsupported platform: %s", runtime.GOOS)
	}

	return cmd.Start()
}
