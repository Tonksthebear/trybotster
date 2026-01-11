// Minimal tsnet connection test
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"tailscale.com/tsnet"
)

func main() {
	// Pre-auth key from Headscale
	authKey := os.Getenv("TS_AUTHKEY")
	if authKey == "" {
		log.Fatal("Set TS_AUTHKEY environment variable")
	}

	// State directory
	homeDir, _ := os.UserHomeDir()
	stateDir := filepath.Join(homeDir, ".botster_hub", "tsnet-test")
	os.MkdirAll(stateDir, 0700)

	// Create tsnet server
	srv := &tsnet.Server{
		Hostname:   "cli-test",
		Dir:        stateDir,
		ControlURL: "http://localhost:8080", // Local Headscale
		AuthKey:    authKey,
		Ephemeral:  true,
	}

	log.Println("Connecting to Headscale...")

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	status, err := srv.Up(ctx)
	if err != nil {
		log.Fatalf("Failed to connect: %v", err)
	}

	fmt.Println("✓ Connected to Headscale!")
	fmt.Printf("  Backend state: %s\n", status.BackendState)
	fmt.Printf("  Tailscale IPs: %v\n", status.TailscaleIPs)

	// Try to listen
	ln, err := srv.Listen("tcp", ":8888")
	if err != nil {
		log.Fatalf("Failed to listen: %v", err)
	}
	fmt.Printf("  Listening on: %s:8888\n", status.TailscaleIPs[0])
	ln.Close()

	fmt.Println("\n✓ tsnet works! Press Ctrl+C to exit.")

	// Wait for signal
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	<-sigCh

	fmt.Println("\nShutting down...")
	srv.Close()
}
