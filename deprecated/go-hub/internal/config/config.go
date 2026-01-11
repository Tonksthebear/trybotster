// Package config provides configuration loading and persistence for botster-hub.
//
// Configuration is loaded from:
// 1. ~/.botster_hub/config.json (file)
// 2. Environment variables (override file values)
//
// Environment variables:
//   - BOTSTER_TOKEN: API authentication token (preferred)
//   - BOTSTER_API_KEY: Legacy API key (deprecated)
//   - BOTSTER_SERVER_URL: Rails server URL
//   - HEADSCALE_URL: Headscale control server URL
//   - BOTSTER_WORKTREE_BASE: Base directory for worktrees
//   - BOTSTER_POLL_INTERVAL: Seconds between polls
//   - BOTSTER_MAX_SESSIONS: Maximum concurrent agents
//   - BOTSTER_AGENT_TIMEOUT: Seconds before idle agent terminates
//   - BOTSTER_CONFIG_DIR: Override config directory (for testing)
package config

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// TokenPrefix is the required prefix for valid authentication tokens.
const TokenPrefix = "btstr_"

// Config holds all configuration for the hub.
type Config struct {
	// ServerURL is the Rails API server URL.
	ServerURL string `json:"server_url"`

	// HeadscaleURL is the Headscale control server URL.
	HeadscaleURL string `json:"headscale_url,omitempty"`

	// Token is the new device token (preferred, must have btstr_ prefix).
	Token string `json:"token,omitempty"`

	// APIKey is the legacy API key (deprecated, kept for backwards compatibility).
	APIKey string `json:"api_key,omitempty"`

	// PollInterval is seconds between message polls.
	PollInterval uint64 `json:"poll_interval"`

	// AgentTimeout is seconds before an idle agent is terminated.
	AgentTimeout uint64 `json:"agent_timeout"`

	// MaxSessions is the maximum concurrent agent sessions.
	MaxSessions int `json:"max_sessions"`

	// WorktreeBase is the directory for git worktrees.
	WorktreeBase string `json:"worktree_base"`
}

// DefaultConfig returns configuration with sensible defaults matching Rust.
func DefaultConfig() *Config {
	homeDir, _ := os.UserHomeDir()
	if homeDir == "" {
		homeDir = "."
	}

	return &Config{
		ServerURL:    "https://trybotster.com",
		HeadscaleURL: "",
		Token:        "",
		APIKey:       "",
		PollInterval: 5,
		AgentTimeout: 3600,
		MaxSessions:  20,
		WorktreeBase: filepath.Join(homeDir, "botster-sessions"),
	}
}

// ConfigDir returns the configuration directory path, creating it if necessary.
// Respects BOTSTER_CONFIG_DIR environment variable for testing.
func ConfigDir() (string, error) {
	// Allow tests to override the config directory
	if testDir := os.Getenv("BOTSTER_CONFIG_DIR"); testDir != "" {
		if err := os.MkdirAll(testDir, 0700); err != nil {
			return "", fmt.Errorf("could not create config directory: %w", err)
		}
		return testDir, nil
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("could not determine home directory: %w", err)
	}

	dir := filepath.Join(homeDir, ".botster_hub")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", fmt.Errorf("could not create config directory: %w", err)
	}

	return dir, nil
}

// ConfigPath returns the path to the config file.
func ConfigPath() (string, error) {
	dir, err := ConfigDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "config.json"), nil
}

// Load reads configuration from file and applies environment variable overrides.
// Priority: Environment variables > config file > defaults
func Load() (*Config, error) {
	cfg := DefaultConfig()

	// Try to load from file
	if err := cfg.loadFromFile(); err != nil {
		// File doesn't exist or is invalid - use defaults
		// This is not an error, we just use defaults
	}

	// Override with environment variables
	cfg.applyEnvOverrides()

	return cfg, nil
}

// loadFromFile attempts to load configuration from the config file.
func (c *Config) loadFromFile() error {
	configPath, err := ConfigPath()
	if err != nil {
		return err
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		return err
	}

	return json.Unmarshal(data, c)
}

// applyEnvOverrides applies environment variable overrides to the config.
func (c *Config) applyEnvOverrides() {
	// Essential config
	if serverURL := os.Getenv("BOTSTER_SERVER_URL"); serverURL != "" {
		c.ServerURL = serverURL
	}

	// Headscale control server URL
	if headscaleURL := os.Getenv("HEADSCALE_URL"); headscaleURL != "" {
		c.HeadscaleURL = headscaleURL
	}

	// New token takes precedence over legacy api_key
	if token := os.Getenv("BOTSTER_TOKEN"); token != "" {
		c.Token = token
	}

	// Legacy api_key support
	if apiKey := os.Getenv("BOTSTER_API_KEY"); apiKey != "" {
		c.APIKey = apiKey
	}

	// Worktree base directory
	if worktreeBase := os.Getenv("BOTSTER_WORKTREE_BASE"); worktreeBase != "" {
		c.WorktreeBase = worktreeBase
	}

	// Optional numeric config
	if pollInterval := os.Getenv("BOTSTER_POLL_INTERVAL"); pollInterval != "" {
		if val, err := strconv.ParseUint(pollInterval, 10, 64); err == nil {
			c.PollInterval = val
		}
	}

	if maxSessions := os.Getenv("BOTSTER_MAX_SESSIONS"); maxSessions != "" {
		if val, err := strconv.Atoi(maxSessions); err == nil {
			c.MaxSessions = val
		}
	}

	if agentTimeout := os.Getenv("BOTSTER_AGENT_TIMEOUT"); agentTimeout != "" {
		if val, err := strconv.ParseUint(agentTimeout, 10, 64); err == nil {
			c.AgentTimeout = val
		}
	}
}

// Save writes configuration to the config file.
func (c *Config) Save() error {
	configPath, err := ConfigPath()
	if err != nil {
		return err
	}

	// Ensure directory exists
	if err := os.MkdirAll(filepath.Dir(configPath), 0700); err != nil {
		return fmt.Errorf("could not create config directory: %w", err)
	}

	data, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return fmt.Errorf("could not marshal config: %w", err)
	}

	if err := os.WriteFile(configPath, data, 0600); err != nil {
		return fmt.Errorf("could not write config file: %w", err)
	}

	return nil
}

// HasToken returns true if we have a valid authentication token.
// Only returns true if the token has the expected btstr_ prefix.
// This ensures legacy api_key values trigger re-authentication.
func (c *Config) HasToken() bool {
	// New token format takes precedence
	if c.Token != "" {
		return strings.HasPrefix(c.Token, TokenPrefix)
	}

	// Legacy api_key - only valid if it happens to have btstr_ prefix (unlikely)
	if c.APIKey != "" {
		return strings.HasPrefix(c.APIKey, TokenPrefix)
	}

	return false
}

// GetAPIKey returns the API key to use for authentication.
// Returns the new device token if set, otherwise falls back to legacy api_key.
func (c *Config) GetAPIKey() string {
	if c.Token != "" {
		return c.Token
	}
	return c.APIKey
}

// SaveToken saves a new device token to the config file.
func (c *Config) SaveToken(token string) error {
	c.Token = token
	return c.Save()
}

// ClearToken clears the token (for logout).
func (c *Config) ClearToken() error {
	c.Token = ""
	return c.Save()
}
