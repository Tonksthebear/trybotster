// Botster Hub - Agent orchestration daemon with embedded Tailscale connectivity.
//
// This is the main entry point for the botster-hub CLI. It manages autonomous
// Claude agents for GitHub issues, providing a TUI for local interaction and
// Tailscale mesh networking for secure browser access.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"
	"github.com/trybotster/botster-hub/internal/config"
	"github.com/trybotster/botster-hub/internal/git"
	"github.com/trybotster/botster-hub/internal/hub"
	"github.com/trybotster/botster-hub/internal/server"
	"github.com/trybotster/botster-hub/internal/tui"
)

// Version is set at build time via ldflags.
var Version = "dev"

func main() {
	// Set up panic recovery to restore terminal on crash
	defer func() {
		if r := recover(); r != nil {
			// Restore terminal - in case we crashed while in raw/alt-screen mode
			// Print escape sequences to restore normal terminal state
			fmt.Print("\033[?1049l") // Exit alt screen
			fmt.Print("\033[?25h")   // Show cursor
			fmt.Print("\033[0m")     // Reset colors

			fmt.Fprintf(os.Stderr, "\n\nPANIC: %v\n", r)
			os.Exit(1)
		}
	}()

	// Set up file logging so TUI doesn't get corrupted by log output
	logFile, err := os.Create("/tmp/botster-hub.log")
	if err != nil {
		fmt.Fprintf(os.Stderr, "Failed to create log file: %v\n", err)
		os.Exit(1)
	}
	defer logFile.Close()

	logLevel := slog.LevelInfo
	if os.Getenv("BOTSTER_LOG_LEVEL") == "debug" {
		logLevel = slog.LevelDebug
	}
	handler := slog.NewTextHandler(logFile, &slog.HandlerOptions{
		Level: logLevel,
	})
	logger := slog.New(handler)
	slog.SetDefault(logger)

	rootCmd := &cobra.Command{
		Use:     "botster-hub",
		Short:   "Agent orchestration daemon for GitHub automation",
		Version: Version,
	}

	// Start command - runs the hub with TUI
	startCmd := &cobra.Command{
		Use:   "start",
		Short: "Start the hub daemon",
		RunE:  runStart,
	}
	startCmd.Flags().Bool("headless", false, "Run without TUI")
	rootCmd.AddCommand(startCmd)

	// Status command
	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show hub status",
		RunE:  runStatus,
	}
	rootCmd.AddCommand(statusCmd)

	// Config command
	configCmd := &cobra.Command{
		Use:   "config",
		Short: "Manage configuration",
		RunE:  runConfig,
	}
	rootCmd.AddCommand(configCmd)

	// json-get command - read JSON config values with dot notation
	jsonGetCmd := &cobra.Command{
		Use:   "json-get <key>",
		Short: "Get a configuration value by dot notation path (e.g., 'server_url')",
		Args:  cobra.ExactArgs(1),
		RunE:  runJSONGet,
	}
	rootCmd.AddCommand(jsonGetCmd)

	// json-set command - set JSON config values with dot notation
	jsonSetCmd := &cobra.Command{
		Use:   "json-set <key> <value>",
		Short: "Set a configuration value by dot notation path",
		Args:  cobra.ExactArgs(2),
		RunE:  runJSONSet,
	}
	rootCmd.AddCommand(jsonSetCmd)

	// json-delete command - delete JSON keys
	jsonDeleteCmd := &cobra.Command{
		Use:   "json-delete <key>",
		Short: "Delete a configuration key",
		Args:  cobra.ExactArgs(1),
		RunE:  runJSONDelete,
	}
	rootCmd.AddCommand(jsonDeleteCmd)

	// list-worktrees command - display all worktrees with info
	listWorktreesCmd := &cobra.Command{
		Use:   "list-worktrees",
		Short: "List all git worktrees with their information",
		RunE:  runListWorktrees,
	}
	rootCmd.AddCommand(listWorktreesCmd)

	// delete-worktree command - remove worktree by issue number
	deleteWorktreeCmd := &cobra.Command{
		Use:   "delete-worktree <issue-number>",
		Short: "Delete a worktree by issue number",
		Args:  cobra.ExactArgs(1),
		RunE:  runDeleteWorktree,
	}
	rootCmd.AddCommand(deleteWorktreeCmd)

	// get-prompt command - get system prompt for worktree
	getPromptCmd := &cobra.Command{
		Use:   "get-prompt <issue-number>",
		Short: "Get the system prompt for a worktree",
		Args:  cobra.ExactArgs(1),
		RunE:  runGetPrompt,
	}
	rootCmd.AddCommand(getPromptCmd)

	// update command - self-update with checksums
	updateCmd := &cobra.Command{
		Use:   "update",
		Short: "Update to the latest version",
		RunE:  runUpdate,
	}
	rootCmd.AddCommand(updateCmd)

	// login command - device flow authentication
	loginCmd := &cobra.Command{
		Use:   "login",
		Short: "Authenticate with the Botster server",
		RunE:  runLogin,
	}
	rootCmd.AddCommand(loginCmd)

	// logout command - clear stored token
	logoutCmd := &cobra.Command{
		Use:   "logout",
		Short: "Clear stored authentication token",
		RunE:  runLogout,
	}
	rootCmd.AddCommand(logoutCmd)

	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func runStart(cmd *cobra.Command, args []string) error {
	headless, _ := cmd.Flags().GetBool("headless")
	logger := slog.Default()

	logger.Info("Starting Botster Hub", "version", Version, "headless", headless)

	// Load configuration
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	// Check for valid token, prompt for login if missing
	if !cfg.HasToken() {
		fmt.Println("No valid authentication token found.")
		fmt.Println("Please authenticate to continue.")
		fmt.Println()

		if err := performDeviceFlowAuth(cfg); err != nil {
			return fmt.Errorf("authentication failed: %w", err)
		}

		// Reload config with new token
		cfg, err = config.Load()
		if err != nil {
			return fmt.Errorf("failed to reload config: %w", err)
		}
	}

	logger.Info("Configuration loaded",
		"server_url", cfg.ServerURL,
		"headscale_url", cfg.HeadscaleURL,
	)

	// Create the hub
	h, err := hub.New(cfg, logger)
	if err != nil {
		return fmt.Errorf("failed to create hub: %w", err)
	}

	// Set up context with cancellation
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// Handle OS signals
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigCh
		logger.Info("Received shutdown signal")
		cancel()
		h.RequestQuit()
	}()

	// Set up the hub (Tailnet connection, SSH server)
	if err := h.Setup(ctx); err != nil {
		logger.Warn("Hub setup had issues", "error", err)
		// Continue anyway - some features may work without Tailnet
	}

	// Start the hub event loop in a goroutine
	go func() {
		if err := h.Run(ctx); err != nil && ctx.Err() == nil {
			logger.Error("Hub event loop error", "error", err)
		}
	}()

	// Run TUI or headless mode
	if headless {
		logger.Info("Running in headless mode")
		// Wait for context cancellation in headless mode
		<-ctx.Done()
	} else {
		// Run the TUI (blocks until quit)
		if err := tui.Run(h); err != nil {
			return fmt.Errorf("TUI error: %w", err)
		}
	}

	// Clean up
	logger.Info("Shutting down...")
	if err := h.Shutdown(); err != nil {
		logger.Error("Shutdown error", "error", err)
	}

	return nil
}

func runStatus(cmd *cobra.Command, args []string) error {
	fmt.Println("Status: not implemented yet")
	return nil
}

func runConfig(cmd *cobra.Command, args []string) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	fmt.Printf("Server URL: %s\n", cfg.ServerURL)
	fmt.Printf("Headscale URL: %s\n", cfg.HeadscaleURL)
	fmt.Printf("Poll Interval: %d seconds\n", cfg.PollInterval)
	fmt.Printf("Max Sessions: %d\n", cfg.MaxSessions)

	return nil
}

func runJSONGet(cmd *cobra.Command, args []string) error {
	key := args[0]

	configPath, err := config.ConfigPath()
	if err != nil {
		return fmt.Errorf("failed to get config path: %w", err)
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("config file does not exist")
		}
		return fmt.Errorf("failed to read config: %w", err)
	}

	var jsonData map[string]interface{}
	if err := json.Unmarshal(data, &jsonData); err != nil {
		return fmt.Errorf("failed to parse config: %w", err)
	}

	value := getJSONValue(jsonData, key)
	if value == nil {
		return fmt.Errorf("key not found: %s", key)
	}

	// Output as JSON if it's a complex type, otherwise as string
	switch v := value.(type) {
	case string:
		fmt.Println(v)
	case float64:
		// Check if it's an integer
		if v == float64(int64(v)) {
			fmt.Printf("%d\n", int64(v))
		} else {
			fmt.Printf("%v\n", v)
		}
	case bool:
		fmt.Printf("%v\n", v)
	default:
		output, _ := json.Marshal(v)
		fmt.Println(string(output))
	}

	return nil
}

func runJSONSet(cmd *cobra.Command, args []string) error {
	key := args[0]
	value := args[1]

	configPath, err := config.ConfigPath()
	if err != nil {
		return fmt.Errorf("failed to get config path: %w", err)
	}

	var jsonData map[string]interface{}

	// Load existing config or start fresh
	data, err := os.ReadFile(configPath)
	if err == nil {
		if err := json.Unmarshal(data, &jsonData); err != nil {
			return fmt.Errorf("failed to parse config: %w", err)
		}
	} else if os.IsNotExist(err) {
		jsonData = make(map[string]interface{})
	} else {
		return fmt.Errorf("failed to read config: %w", err)
	}

	// Parse value - try as number, bool, then string
	var parsedValue interface{}
	if intVal, err := strconv.ParseInt(value, 10, 64); err == nil {
		parsedValue = intVal
	} else if floatVal, err := strconv.ParseFloat(value, 64); err == nil {
		parsedValue = floatVal
	} else if value == "true" {
		parsedValue = true
	} else if value == "false" {
		parsedValue = false
	} else {
		parsedValue = value
	}

	setJSONValue(jsonData, key, parsedValue)

	// Write back
	output, err := json.MarshalIndent(jsonData, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal config: %w", err)
	}

	if err := os.WriteFile(configPath, output, 0600); err != nil {
		return fmt.Errorf("failed to write config: %w", err)
	}

	fmt.Printf("Set %s = %v\n", key, parsedValue)
	return nil
}

func runJSONDelete(cmd *cobra.Command, args []string) error {
	key := args[0]

	configPath, err := config.ConfigPath()
	if err != nil {
		return fmt.Errorf("failed to get config path: %w", err)
	}

	data, err := os.ReadFile(configPath)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("config file does not exist")
		}
		return fmt.Errorf("failed to read config: %w", err)
	}

	var jsonData map[string]interface{}
	if err := json.Unmarshal(data, &jsonData); err != nil {
		return fmt.Errorf("failed to parse config: %w", err)
	}

	if !deleteJSONValue(jsonData, key) {
		return fmt.Errorf("key not found: %s", key)
	}

	// Write back
	output, err := json.MarshalIndent(jsonData, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal config: %w", err)
	}

	if err := os.WriteFile(configPath, output, 0600); err != nil {
		return fmt.Errorf("failed to write config: %w", err)
	}

	fmt.Printf("Deleted %s\n", key)
	return nil
}

func runListWorktrees(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("failed to get current directory: %w", err)
	}

	logger := slog.Default()
	gitMgr := git.New(cwd, logger)

	worktrees, err := gitMgr.ListAllWorktrees()
	if err != nil {
		return fmt.Errorf("failed to list worktrees: %w", err)
	}

	if len(worktrees) == 0 {
		fmt.Println("No worktrees found")
		return nil
	}

	for _, wt := range worktrees {
		fmt.Printf("%s\t%s\n", wt.Path, wt.Branch)
	}

	return nil
}

func runDeleteWorktree(cmd *cobra.Command, args []string) error {
	issueNum, err := strconv.Atoi(args[0])
	if err != nil {
		return fmt.Errorf("invalid issue number: %w", err)
	}

	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("failed to get current directory: %w", err)
	}

	logger := slog.Default()
	gitMgr := git.New(cwd, logger)

	// Find worktree by issue number
	worktrees, err := gitMgr.ListAllWorktrees()
	if err != nil {
		return fmt.Errorf("failed to list worktrees: %w", err)
	}

	branchName := fmt.Sprintf("botster-issue-%d", issueNum)
	var targetWorktree *git.Worktree
	for _, wt := range worktrees {
		if wt.Branch == branchName {
			targetWorktree = wt
			break
		}
	}

	if targetWorktree == nil {
		return fmt.Errorf("worktree for issue %d not found", issueNum)
	}

	if err := gitMgr.DeleteWorktreeByPath(targetWorktree.Path, branchName); err != nil {
		return fmt.Errorf("failed to delete worktree: %w", err)
	}

	fmt.Printf("Deleted worktree for issue %d at %s\n", issueNum, targetWorktree.Path)
	return nil
}

func runGetPrompt(cmd *cobra.Command, args []string) error {
	issueNum, err := strconv.Atoi(args[0])
	if err != nil {
		return fmt.Errorf("invalid issue number: %w", err)
	}

	cwd, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("failed to get current directory: %w", err)
	}

	logger := slog.Default()
	gitMgr := git.New(cwd, logger)

	// Find worktree by issue number
	worktrees, err := gitMgr.ListAllWorktrees()
	if err != nil {
		return fmt.Errorf("failed to list worktrees: %w", err)
	}

	branchName := fmt.Sprintf("botster-issue-%d", issueNum)
	var targetWorktree *git.Worktree
	for _, wt := range worktrees {
		if wt.Branch == branchName {
			targetWorktree = wt
			break
		}
	}

	if targetWorktree == nil {
		return fmt.Errorf("worktree for issue %d not found", issueNum)
	}

	// Read .botster_prompt from worktree
	promptPath := filepath.Join(targetWorktree.Path, ".botster_prompt")
	data, err := os.ReadFile(promptPath)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("no prompt file found for issue %d", issueNum)
		}
		return fmt.Errorf("failed to read prompt: %w", err)
	}

	fmt.Print(string(data))
	return nil
}

func runUpdate(cmd *cobra.Command, args []string) error {
	// For now, just print instructions since auto-update requires server integration
	fmt.Println("Update functionality requires server integration.")
	fmt.Println("")
	fmt.Println("To manually update:")
	fmt.Println("  curl -L https://trybotster.com/downloads/botster-hub -o /usr/local/bin/botster-hub")
	fmt.Println("  chmod +x /usr/local/bin/botster-hub")
	fmt.Println("")
	fmt.Printf("Current version: %s\n", Version)

	return nil
}

func runLogin(cmd *cobra.Command, args []string) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	// Check if already logged in
	if cfg.HasToken() {
		fmt.Println("Already logged in.")
		fmt.Println("Run 'botster-hub logout' to clear the stored token.")
		return nil
	}

	return performDeviceFlowAuth(cfg)
}

func runLogout(cmd *cobra.Command, args []string) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	if err := cfg.ClearToken(); err != nil {
		return fmt.Errorf("failed to clear token: %w", err)
	}

	fmt.Println("Logged out successfully.")
	return nil
}

// performDeviceFlowAuth runs the OAuth device flow authentication.
func performDeviceFlowAuth(cfg *config.Config) error {
	ctx := context.Background()

	fmt.Println("Authenticating with Botster...")
	fmt.Println()

	// Request device code
	deviceCode, err := server.RequestDeviceCode(ctx, cfg.ServerURL)
	if err != nil {
		return fmt.Errorf("failed to request device code: %w", err)
	}

	fmt.Printf("Please visit: %s\n", deviceCode.VerificationURL)
	fmt.Printf("And enter code: %s\n", deviceCode.UserCode)
	fmt.Println()
	fmt.Println("Waiting for authorization...")

	// Poll for token
	interval := time.Duration(deviceCode.Interval) * time.Second
	if interval < time.Second {
		interval = 5 * time.Second
	}

	deadline := time.Now().Add(time.Duration(deviceCode.ExpiresIn) * time.Second)

	for time.Now().Before(deadline) {
		time.Sleep(interval)

		tokenResp, err := server.PollDeviceToken(ctx, cfg.ServerURL, deviceCode.DeviceCode)
		if err != nil {
			return fmt.Errorf("failed to poll for token: %w", err)
		}

		switch tokenResp.Error {
		case "":
			// Success!
			if err := cfg.SaveToken(tokenResp.AccessToken); err != nil {
				return fmt.Errorf("failed to save token: %w", err)
			}
			fmt.Println("Successfully authenticated!")
			return nil
		case "authorization_pending":
			// Keep polling
			continue
		case "slow_down":
			interval += 5 * time.Second
			continue
		case "expired_token":
			return fmt.Errorf("authorization expired, please try again")
		case "access_denied":
			return fmt.Errorf("authorization denied by user")
		default:
			return fmt.Errorf("authorization failed: %s", tokenResp.Error)
		}
	}

	return fmt.Errorf("authorization timed out")
}

// JSON helper functions for dot notation

func getJSONValue(data map[string]interface{}, path string) interface{} {
	parts := strings.Split(path, ".")
	current := interface{}(data)

	for _, part := range parts {
		switch v := current.(type) {
		case map[string]interface{}:
			var ok bool
			current, ok = v[part]
			if !ok {
				return nil
			}
		default:
			return nil
		}
	}

	return current
}

func setJSONValue(data map[string]interface{}, path string, value interface{}) {
	parts := strings.Split(path, ".")

	if len(parts) == 1 {
		data[path] = value
		return
	}

	current := data
	for i := 0; i < len(parts)-1; i++ {
		part := parts[i]
		if _, ok := current[part]; !ok {
			current[part] = make(map[string]interface{})
		}
		if nested, ok := current[part].(map[string]interface{}); ok {
			current = nested
		} else {
			return
		}
	}

	current[parts[len(parts)-1]] = value
}

func deleteJSONValue(data map[string]interface{}, path string) bool {
	parts := strings.Split(path, ".")

	if len(parts) == 1 {
		if _, ok := data[path]; ok {
			delete(data, path)
			return true
		}
		return false
	}

	current := data
	for i := 0; i < len(parts)-1; i++ {
		if nested, ok := current[parts[i]].(map[string]interface{}); ok {
			current = nested
		} else {
			return false
		}
	}

	key := parts[len(parts)-1]
	if _, ok := current[key]; ok {
		delete(current, key)
		return true
	}
	return false
}
