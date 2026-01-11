package config

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// setupTestEnv creates a temporary config directory and clears env vars.
// Returns cleanup function to restore state.
func setupTestEnv(t *testing.T) func() {
	t.Helper()

	// Save original env vars
	origConfigDir := os.Getenv("BOTSTER_CONFIG_DIR")
	origServerURL := os.Getenv("BOTSTER_SERVER_URL")
	origToken := os.Getenv("BOTSTER_TOKEN")
	origAPIKey := os.Getenv("BOTSTER_API_KEY")
	origHeadscale := os.Getenv("HEADSCALE_URL")
	origWorktree := os.Getenv("BOTSTER_WORKTREE_BASE")
	origPoll := os.Getenv("BOTSTER_POLL_INTERVAL")
	origMaxSessions := os.Getenv("BOTSTER_MAX_SESSIONS")
	origTimeout := os.Getenv("BOTSTER_AGENT_TIMEOUT")

	// Create temp directory for config
	tmpDir := t.TempDir()
	os.Setenv("BOTSTER_CONFIG_DIR", tmpDir)

	// Clear other env vars
	os.Unsetenv("BOTSTER_SERVER_URL")
	os.Unsetenv("BOTSTER_TOKEN")
	os.Unsetenv("BOTSTER_API_KEY")
	os.Unsetenv("HEADSCALE_URL")
	os.Unsetenv("BOTSTER_WORKTREE_BASE")
	os.Unsetenv("BOTSTER_POLL_INTERVAL")
	os.Unsetenv("BOTSTER_MAX_SESSIONS")
	os.Unsetenv("BOTSTER_AGENT_TIMEOUT")

	return func() {
		os.Setenv("BOTSTER_CONFIG_DIR", origConfigDir)
		if origServerURL != "" {
			os.Setenv("BOTSTER_SERVER_URL", origServerURL)
		}
		if origToken != "" {
			os.Setenv("BOTSTER_TOKEN", origToken)
		}
		if origAPIKey != "" {
			os.Setenv("BOTSTER_API_KEY", origAPIKey)
		}
		if origHeadscale != "" {
			os.Setenv("HEADSCALE_URL", origHeadscale)
		}
		if origWorktree != "" {
			os.Setenv("BOTSTER_WORKTREE_BASE", origWorktree)
		}
		if origPoll != "" {
			os.Setenv("BOTSTER_POLL_INTERVAL", origPoll)
		}
		if origMaxSessions != "" {
			os.Setenv("BOTSTER_MAX_SESSIONS", origMaxSessions)
		}
		if origTimeout != "" {
			os.Setenv("BOTSTER_AGENT_TIMEOUT", origTimeout)
		}
	}
}

func TestDefaultConfig(t *testing.T) {
	cfg := DefaultConfig()

	if cfg.ServerURL != "https://trybotster.com" {
		t.Errorf("ServerURL = %q, want %q", cfg.ServerURL, "https://trybotster.com")
	}
	if cfg.PollInterval != 5 {
		t.Errorf("PollInterval = %d, want %d", cfg.PollInterval, 5)
	}
	if cfg.MaxSessions != 20 {
		t.Errorf("MaxSessions = %d, want %d", cfg.MaxSessions, 20)
	}
	if cfg.AgentTimeout != 3600 {
		t.Errorf("AgentTimeout = %d, want %d", cfg.AgentTimeout, 3600)
	}
	if cfg.HeadscaleURL != "" {
		t.Errorf("HeadscaleURL = %q, want empty", cfg.HeadscaleURL)
	}
}

func TestConfigSerialization(t *testing.T) {
	cfg := DefaultConfig()
	cfg.Token = "btstr_test123"
	cfg.HeadscaleURL = "http://localhost:8080"

	data, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("Marshal failed: %v", err)
	}

	var loaded Config
	if err := json.Unmarshal(data, &loaded); err != nil {
		t.Fatalf("Unmarshal failed: %v", err)
	}

	if loaded.ServerURL != cfg.ServerURL {
		t.Errorf("ServerURL = %q, want %q", loaded.ServerURL, cfg.ServerURL)
	}
	if loaded.Token != cfg.Token {
		t.Errorf("Token = %q, want %q", loaded.Token, cfg.Token)
	}
	if loaded.HeadscaleURL != cfg.HeadscaleURL {
		t.Errorf("HeadscaleURL = %q, want %q", loaded.HeadscaleURL, cfg.HeadscaleURL)
	}
}

func TestGetAPIKeyPrefersToken(t *testing.T) {
	cfg := &Config{
		APIKey: "legacy_key",
		Token:  "new_token",
	}

	if got := cfg.GetAPIKey(); got != "new_token" {
		t.Errorf("GetAPIKey() = %q, want %q", got, "new_token")
	}
}

func TestGetAPIKeyFallsBackToAPIKey(t *testing.T) {
	cfg := &Config{
		APIKey: "legacy_key",
		Token:  "",
	}

	if got := cfg.GetAPIKey(); got != "legacy_key" {
		t.Errorf("GetAPIKey() = %q, want %q", got, "legacy_key")
	}
}

func TestHasToken(t *testing.T) {
	tests := []struct {
		name   string
		token  string
		apiKey string
		want   bool
	}{
		{
			name:   "empty token and api_key",
			token:  "",
			apiKey: "",
			want:   false,
		},
		{
			name:   "valid token with btstr_ prefix",
			token:  "btstr_token123",
			apiKey: "",
			want:   true,
		},
		{
			name:   "token without btstr_ prefix is invalid",
			token:  "invalid_token",
			apiKey: "",
			want:   false,
		},
		{
			name:   "legacy api_key without prefix is invalid",
			token:  "",
			apiKey: "legacy_key",
			want:   false,
		},
		{
			name:   "legacy api_key with btstr_ prefix is valid (edge case)",
			token:  "",
			apiKey: "btstr_legacy_key",
			want:   true,
		},
		{
			name:   "token takes precedence over api_key",
			token:  "btstr_new",
			apiKey: "legacy",
			want:   true,
		},
		{
			name:   "invalid token with valid api_key uses token check first",
			token:  "invalid",
			apiKey: "btstr_legacy",
			want:   false, // Token is checked first and doesn't have prefix
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := &Config{
				Token:  tt.token,
				APIKey: tt.apiKey,
			}
			if got := cfg.HasToken(); got != tt.want {
				t.Errorf("HasToken() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestLoadFromFile(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Create a config file
	configPath, err := ConfigPath()
	if err != nil {
		t.Fatalf("ConfigPath() failed: %v", err)
	}

	fileConfig := &Config{
		ServerURL:    "https://custom.server.com",
		Token:        "btstr_file_token",
		PollInterval: 10,
		MaxSessions:  5,
		AgentTimeout: 1800,
		WorktreeBase: "/custom/worktrees",
	}

	data, err := json.MarshalIndent(fileConfig, "", "  ")
	if err != nil {
		t.Fatalf("Marshal failed: %v", err)
	}

	if err := os.WriteFile(configPath, data, 0600); err != nil {
		t.Fatalf("WriteFile failed: %v", err)
	}

	// Load config
	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	if cfg.ServerURL != "https://custom.server.com" {
		t.Errorf("ServerURL = %q, want %q", cfg.ServerURL, "https://custom.server.com")
	}
	if cfg.Token != "btstr_file_token" {
		t.Errorf("Token = %q, want %q", cfg.Token, "btstr_file_token")
	}
	if cfg.PollInterval != 10 {
		t.Errorf("PollInterval = %d, want %d", cfg.PollInterval, 10)
	}
}

func TestEnvOverridesFile(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Create a config file
	configPath, err := ConfigPath()
	if err != nil {
		t.Fatalf("ConfigPath() failed: %v", err)
	}

	fileConfig := &Config{
		ServerURL:    "https://file.server.com",
		Token:        "btstr_file_token",
		PollInterval: 10,
	}

	data, _ := json.MarshalIndent(fileConfig, "", "  ")
	os.WriteFile(configPath, data, 0600)

	// Set env vars to override
	os.Setenv("BOTSTER_SERVER_URL", "https://env.server.com")
	os.Setenv("BOTSTER_TOKEN", "btstr_env_token")
	os.Setenv("BOTSTER_POLL_INTERVAL", "30")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	// Env should override file
	if cfg.ServerURL != "https://env.server.com" {
		t.Errorf("ServerURL = %q, want %q (env override)", cfg.ServerURL, "https://env.server.com")
	}
	if cfg.Token != "btstr_env_token" {
		t.Errorf("Token = %q, want %q (env override)", cfg.Token, "btstr_env_token")
	}
	if cfg.PollInterval != 30 {
		t.Errorf("PollInterval = %d, want %d (env override)", cfg.PollInterval, 30)
	}
}

func TestAllEnvOverrides(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Set all env vars
	os.Setenv("BOTSTER_SERVER_URL", "https://env.server.com")
	os.Setenv("BOTSTER_TOKEN", "btstr_env_token")
	os.Setenv("BOTSTER_API_KEY", "legacy_key")
	os.Setenv("HEADSCALE_URL", "http://headscale:8080")
	os.Setenv("BOTSTER_WORKTREE_BASE", "/env/worktrees")
	os.Setenv("BOTSTER_POLL_INTERVAL", "15")
	os.Setenv("BOTSTER_MAX_SESSIONS", "50")
	os.Setenv("BOTSTER_AGENT_TIMEOUT", "7200")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	if cfg.ServerURL != "https://env.server.com" {
		t.Errorf("ServerURL = %q, want %q", cfg.ServerURL, "https://env.server.com")
	}
	if cfg.Token != "btstr_env_token" {
		t.Errorf("Token = %q, want %q", cfg.Token, "btstr_env_token")
	}
	if cfg.APIKey != "legacy_key" {
		t.Errorf("APIKey = %q, want %q", cfg.APIKey, "legacy_key")
	}
	if cfg.HeadscaleURL != "http://headscale:8080" {
		t.Errorf("HeadscaleURL = %q, want %q", cfg.HeadscaleURL, "http://headscale:8080")
	}
	if cfg.WorktreeBase != "/env/worktrees" {
		t.Errorf("WorktreeBase = %q, want %q", cfg.WorktreeBase, "/env/worktrees")
	}
	if cfg.PollInterval != 15 {
		t.Errorf("PollInterval = %d, want %d", cfg.PollInterval, 15)
	}
	if cfg.MaxSessions != 50 {
		t.Errorf("MaxSessions = %d, want %d", cfg.MaxSessions, 50)
	}
	if cfg.AgentTimeout != 7200 {
		t.Errorf("AgentTimeout = %d, want %d", cfg.AgentTimeout, 7200)
	}
}

func TestSaveAndLoad(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Create and save config
	cfg := DefaultConfig()
	cfg.Token = "btstr_saved_token"
	cfg.HeadscaleURL = "http://saved.headscale:8080"

	if err := cfg.Save(); err != nil {
		t.Fatalf("Save() failed: %v", err)
	}

	// Load it back
	loaded, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	if loaded.Token != "btstr_saved_token" {
		t.Errorf("Token = %q, want %q", loaded.Token, "btstr_saved_token")
	}
	if loaded.HeadscaleURL != "http://saved.headscale:8080" {
		t.Errorf("HeadscaleURL = %q, want %q", loaded.HeadscaleURL, "http://saved.headscale:8080")
	}
}

func TestSaveToken(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	cfg := DefaultConfig()

	if err := cfg.SaveToken("btstr_new_token"); err != nil {
		t.Fatalf("SaveToken() failed: %v", err)
	}

	// Verify token was saved
	if cfg.Token != "btstr_new_token" {
		t.Errorf("Token = %q, want %q", cfg.Token, "btstr_new_token")
	}

	// Load fresh config to verify persistence
	loaded, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	if loaded.Token != "btstr_new_token" {
		t.Errorf("Loaded Token = %q, want %q", loaded.Token, "btstr_new_token")
	}
}

func TestClearToken(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	cfg := DefaultConfig()
	cfg.Token = "btstr_to_clear"
	cfg.Save()

	if err := cfg.ClearToken(); err != nil {
		t.Fatalf("ClearToken() failed: %v", err)
	}

	if cfg.Token != "" {
		t.Errorf("Token = %q, want empty", cfg.Token)
	}

	// Verify cleared in file
	loaded, _ := Load()
	if loaded.Token != "" {
		t.Errorf("Loaded Token = %q, want empty", loaded.Token)
	}
}

func TestConfigDirOverride(t *testing.T) {
	tmpDir := t.TempDir()
	customDir := filepath.Join(tmpDir, "custom_config")

	os.Setenv("BOTSTER_CONFIG_DIR", customDir)
	defer os.Unsetenv("BOTSTER_CONFIG_DIR")

	dir, err := ConfigDir()
	if err != nil {
		t.Fatalf("ConfigDir() failed: %v", err)
	}

	if dir != customDir {
		t.Errorf("ConfigDir() = %q, want %q", dir, customDir)
	}

	// Verify directory was created
	if _, err := os.Stat(customDir); os.IsNotExist(err) {
		t.Errorf("Config directory was not created")
	}
}

func TestLoadWithNoFile(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Don't create any config file - should use defaults
	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	// Should have default values
	if cfg.ServerURL != "https://trybotster.com" {
		t.Errorf("ServerURL = %q, want default", cfg.ServerURL)
	}
	if cfg.PollInterval != 5 {
		t.Errorf("PollInterval = %d, want default 5", cfg.PollInterval)
	}
}

func TestInvalidEnvVarsIgnored(t *testing.T) {
	cleanup := setupTestEnv(t)
	defer cleanup()

	// Set invalid numeric values
	os.Setenv("BOTSTER_POLL_INTERVAL", "not_a_number")
	os.Setenv("BOTSTER_MAX_SESSIONS", "invalid")
	os.Setenv("BOTSTER_AGENT_TIMEOUT", "")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	// Should keep defaults when env values are invalid
	if cfg.PollInterval != 5 {
		t.Errorf("PollInterval = %d, want default 5 (invalid env ignored)", cfg.PollInterval)
	}
	if cfg.MaxSessions != 20 {
		t.Errorf("MaxSessions = %d, want default 20 (invalid env ignored)", cfg.MaxSessions)
	}
	if cfg.AgentTimeout != 3600 {
		t.Errorf("AgentTimeout = %d, want default 3600 (empty env ignored)", cfg.AgentTimeout)
	}
}
