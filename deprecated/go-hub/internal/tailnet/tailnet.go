// Package tailnet provides Tailscale mesh networking via tsnet.
//
// This package wraps tsnet to provide embedded Tailscale connectivity
// for the botster-hub. It connects to a Headscale control server and
// enables secure browser access via Tailscale SSH.
//
// Key features:
//   - Zero external dependencies (no tailscale binary needed)
//   - Userspace networking (no root/admin required)
//   - Direct integration with Headscale via ControlURL
//   - SSH server for browser terminal access
package tailnet

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"

	"tailscale.com/tsnet"
)

// Client wraps a tsnet.Server for Headscale connectivity.
type Client struct {
	server *tsnet.Server
	hubID  string
	logger *slog.Logger
}

// Config holds configuration for the Tailnet client.
type Config struct {
	// HubID is the unique identifier for this hub instance.
	HubID string

	// HeadscaleURL is the control server URL (e.g., "https://headscale.trybotster.com").
	HeadscaleURL string

	// AuthKey is the pre-auth key for joining the tailnet.
	AuthKey string

	// StateDir is the directory for storing Tailscale state.
	// Defaults to ~/.botster_hub/tsnet/<hubID>
	StateDir string

	// Ephemeral indicates whether this node should be ephemeral.
	Ephemeral bool
}

// New creates a new Tailnet client.
func New(cfg *Config, logger *slog.Logger) (*Client, error) {
	if cfg.HubID == "" {
		return nil, fmt.Errorf("HubID is required")
	}
	if cfg.HeadscaleURL == "" {
		return nil, fmt.Errorf("HeadscaleURL is required")
	}

	// Default state directory
	stateDir := cfg.StateDir
	if stateDir == "" {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			return nil, fmt.Errorf("could not determine home directory: %w", err)
		}
		stateDir = filepath.Join(homeDir, ".botster_hub", "tsnet", cfg.HubID)
	}

	// Ensure state directory exists
	if err := os.MkdirAll(stateDir, 0700); err != nil {
		return nil, fmt.Errorf("could not create state directory: %w", err)
	}

	hostname := fmt.Sprintf("cli-%s", cfg.HubID[:8]) // Use first 8 chars of hub ID

	server := &tsnet.Server{
		Hostname:   hostname,
		Dir:        stateDir,
		ControlURL: cfg.HeadscaleURL,
		AuthKey:    cfg.AuthKey,
		Ephemeral:  cfg.Ephemeral,
		Logf:       func(format string, args ...any) { logger.Debug(fmt.Sprintf(format, args...)) },
	}

	return &Client{
		server: server,
		hubID:  cfg.HubID,
		logger: logger,
	}, nil
}

// Start connects to the Tailscale network.
func (c *Client) Start(ctx context.Context) error {
	c.logger.Info("Connecting to Tailscale network",
		"hostname", c.server.Hostname,
		"control_url", c.server.ControlURL,
	)

	status, err := c.server.Up(ctx)
	if err != nil {
		return fmt.Errorf("failed to connect to tailnet: %w", err)
	}

	c.logger.Info("Connected to Tailscale network",
		"tailscale_ips", status.TailscaleIPs,
		"backend_state", status.BackendState,
	)

	return nil
}

// Close shuts down the Tailscale connection.
func (c *Client) Close() error {
	c.logger.Info("Disconnecting from Tailscale network")
	return c.server.Close()
}

// Listen creates a TCP listener on the tailnet.
func (c *Client) Listen(network, addr string) (net.Listener, error) {
	return c.server.Listen(network, addr)
}

// Dial connects to an address on the tailnet.
func (c *Client) Dial(ctx context.Context, network, addr string) (net.Conn, error) {
	return c.server.Dial(ctx, network, addr)
}

// TailscaleIPs returns the Tailscale IP addresses for this node.
// Returns IPv4 and IPv6 addresses as strings.
func (c *Client) TailscaleIPs() []string {
	ip4, ip6 := c.server.TailscaleIPs()
	var result []string
	if ip4.IsValid() {
		result = append(result, ip4.String())
	}
	if ip6.IsValid() {
		result = append(result, ip6.String())
	}
	return result
}

// Hostname returns the tailnet hostname.
func (c *Client) Hostname() string {
	return c.server.Hostname
}
