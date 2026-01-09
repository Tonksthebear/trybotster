module github.com/trybotster/botster-hub

go 1.23

require (
	// Networking - tsnet for embedded Tailscale
	tailscale.com v1.76.6

	// TUI - Bubble Tea ecosystem
	github.com/charmbracelet/bubbletea v1.2.4
	github.com/charmbracelet/bubbles v0.20.0
	github.com/charmbracelet/lipgloss v1.0.0

	// PTY - pseudo-terminal handling
	github.com/creack/pty v1.1.24

	// VT100 - terminal emulation
	github.com/vito/vt100 v0.0.0-20230623065551-f95fc1fbe8a9

	// Git - pure Go git implementation
	github.com/go-git/go-git/v5 v5.13.1

	// CLI - command line parsing
	github.com/spf13/cobra v1.8.1

	// Utilities
	github.com/google/uuid v1.6.0
	github.com/skip2/go-qrcode v0.0.0-20200617195104-da1b6568686e
	github.com/zalando/go-keyring v0.2.6
	github.com/atotto/clipboard v0.1.4

	// SSH server for browser terminal
	github.com/gliderlabs/ssh v0.3.8
	golang.org/x/crypto v0.31.0
)
