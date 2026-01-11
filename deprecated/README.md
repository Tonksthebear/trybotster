# Deprecated: Go CLI + Tailscale/Headscale Experiment

This folder contains code from an experiment to rewrite the Rust CLI in Go and use Tailscale/Headscale for mesh networking between CLI and browser.

## Why Deprecated

The Go rewrite was motivated by wanting to use **tsnet** (Tailscale's embeddable Go library) for direct mesh networking. However, we discovered that **Headscale doesn't support per-user scoped API keys** - there's only a single admin API key.

This breaks our trust model:
- We wanted users to only be able to create keys for their own namespace
- Headscale's single admin key would give users access to create keys for ANY namespace
- Building a proxy/key-service adds complexity without solving the fundamental trust issue

## What We're Using Instead

Going back to the **Rust CLI with Signal Protocol E2E encryption**:
- No coordination server needed (eliminates trust issue)
- QR code contains public keys (safe to share)
- Rails is a pure relay that can't decrypt anything
- Post-quantum ready with Kyber support
- Already implemented

## Contents

```
deprecated/
├── go-hub/                    # Full Go CLI implementation
│   ├── cmd/                   # CLI commands
│   ├── internal/              # Core packages
│   │   ├── hub/               # Hub state management
│   │   ├── agent/             # Agent/PTY management
│   │   ├── tui/               # tcell TUI
│   │   ├── tailnet/           # tsnet wrapper
│   │   ├── vt100/             # Terminal emulator (charmbracelet/x/vt)
│   │   └── ...
│   └── go.mod
├── app/
│   ├── controllers/hubs/
│   │   └── tailscale_controller.rb
│   ├── services/
│   │   └── headscale_client.rb
│   └── javascript/
│       └── tailscale_client.js
└── docs/
    ├── headscale-browser-architecture.md
    ├── headscale-plan.md
    └── headscale-implementation.md
```

## Learnings

1. **Tailscale/Headscale is great for infrastructure you control** - single admin model works well
2. **Not designed for multi-tenant self-service** - no per-user API scoping
3. **Signal E2E is simpler for our use case** - no coordination server, pure relay model
4. **Go rewrite clarified architecture** - valuable exploration, not wasted work

## Date Deprecated

January 2026
