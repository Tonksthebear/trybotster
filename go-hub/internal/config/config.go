// Package config provides configuration loading for botster-hub.
//
// Configuration is loaded from:
// 1. ~/.botster_hub/config.json (file)
// 2. Environment variables (override file values)
//
// Environment variables:
//   - BOTSTER_TOKEN or BOTSTER_API_KEY: API authentication token
//   - HOST_URL: Rails server URL
//   - HEADSCALE_URL: Headscale control server URL
package config

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// Config holds all configuration for the hub.
type Config struct {
	// ServerURL is the Rails API server URL.
	ServerURL string `json:"server_url"`

	// Token is the API authentication token.
	Token string `json:"token"`

	// HeadscaleURL is the Headscale control server URL.
	HeadscaleURL string `json:"headscale_url"`

	// PollInterval is seconds between message polls.
	PollInterval int `json:"poll_interval"`

	// AgentTimeout is seconds before an idle agent is terminated.
	AgentTimeout int `json:"agent_timeout"`

	// MaxSessions is the maximum concurrent agent sessions.
	MaxSessions int `json:"max_sessions"`

	// WorktreeBase is the directory for git worktrees.
	WorktreeBase string `json:"worktree_base"`
}

// DefaultConfig returns configuration with sensible defaults.
func DefaultConfig() *Config {
	homeDir, _ := os.UserHomeDir()

	return &Config{
		ServerURL:    "https://trybotster.com",
		HeadscaleURL: "",
		PollInterval: 5,
		AgentTimeout: 300,
		MaxSessions:  10,
		WorktreeBase: filepath.Join(homeDir, ".botster_hub", "worktrees"),
	}
}

// ConfigPath returns the path to the config file.
func ConfigPath() (string, error) {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("could not determine home directory: %w", err)
	}
	return filepath.Join(homeDir, ".botster_hub", "config.json"), nil
}

// Load reads configuration from file and environment variables.
func Load() (*Config, error) {
	cfg := DefaultConfig()

	// Try to load from file
	configPath, err := ConfigPath()
	if err != nil {
		return nil, err
	}

	if data, err := os.ReadFile(configPath); err == nil {
		if err := json.Unmarshal(data, cfg); err != nil {
			return nil, fmt.Errorf("invalid config file: %w", err)
		}
	}

	// Override with environment variables
	if url := os.Getenv("HOST_URL"); url != "" {
		cfg.ServerURL = "https://" + url
	}

	if token := os.Getenv("BOTSTER_TOKEN"); token != "" {
		cfg.Token = token
	} else if apiKey := os.Getenv("BOTSTER_API_KEY"); apiKey != "" {
		cfg.Token = apiKey
	}

	if headscale := os.Getenv("HEADSCALE_URL"); headscale != "" {
		cfg.HeadscaleURL = headscale
	}

	return cfg, nil
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

// HasToken returns true if an API token is configured.
func (c *Config) HasToken() bool {
	return c.Token != ""
}

// GetAPIKey returns the API key for authentication.
func (c *Config) GetAPIKey() string {
	return c.Token
}
