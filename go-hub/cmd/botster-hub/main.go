// Botster Hub - Agent orchestration daemon with embedded Tailscale connectivity.
//
// This is the main entry point for the botster-hub CLI. It manages autonomous
// Claude agents for GitHub issues, providing a TUI for local interaction and
// Tailscale mesh networking for secure browser access.
package main

import (
	"fmt"
	"log/slog"
	"os"

	"github.com/spf13/cobra"
	"github.com/trybotster/botster-hub/internal/config"
)

// Version is set at build time via ldflags.
var Version = "dev"

func main() {
	// Set up structured logging
	handler := slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{
		Level: slog.LevelInfo,
	})
	slog.SetDefault(slog.New(handler))

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

	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func runStart(cmd *cobra.Command, args []string) error {
	headless, _ := cmd.Flags().GetBool("headless")

	slog.Info("Starting Botster Hub", "version", Version, "headless", headless)

	// Load configuration
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	slog.Info("Configuration loaded",
		"server_url", cfg.ServerURL,
		"headscale_url", cfg.HeadscaleURL,
	)

	// TODO: Initialize hub
	// TODO: Connect to Tailscale/Headscale via tsnet
	// TODO: Start TUI or headless mode
	// TODO: Run event loop

	fmt.Println("Hub started successfully (stub)")
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
