# Botster Architecture

Comprehensive documentation of Botster's architecture, design decisions, and security model.

## Overview

Botster enables GitHub-mention-triggered autonomous agents running on local machines with secure browser-based terminal access.

```
GitHub @mention → Rails server → Message queue → Rust CLI polls
                                                      ↓
                                              Creates git worktree
                                                      ↓
                                              Spawns Claude in PTY
                                                      ↓
                              Browser ← E2E encrypted → CLI terminal
```

## Components

### Rails Server (trybotster.com)

**Role:** Message routing, user auth, relay for encrypted communication.

**Key principle:** The server is a **pure relay** for terminal communication. It **cannot decrypt** any terminal content due to E2E encryption.

**Responsibilities:**

- GitHub OAuth authentication
- GitHub webhook processing (creates `Integrations::Github::Message` records)
- User/Hub/Device record management
- Action Cable WebSocket relay (TerminalRelayChannel)
- ActionCable delivery of hub commands and GitHub events to CLI

**Does NOT:**

- Store encryption keys or bundles
- Decrypt any terminal content
- Have access to agent conversations

### Rust CLI (botster-hub)

**Role:** Local daemon that manages agents and PTY sessions.

**Location:** `cli/` directory

**Responsibilities:**

- Polls Rails for messages
- Creates/manages git worktrees per issue
- Spawns Claude in PTY with full terminal emulation
- Signal Protocol encryption for browser communication
- TUI rendering with ratatui
- Session persistence to OS keyring

**Key files:**

- `cli/src/hub/` - Hub lifecycle, registration, polling
- `cli/src/agent/` - PTY spawning and management
- `cli/src/relay/` - WebSocket relay with Signal encryption
- `cli/src/tui/` - Terminal UI rendering

### Browser Client

**Role:** Secure terminal access to CLI agents.

**Key principle:** All terminal data is E2E encrypted. Browser decrypts locally using Signal Protocol WASM running in an isolated Web Worker.

**Responsibilities:**

- Signal Protocol session management (WASM in Web Worker)
- Action Cable subscription for relay
- Terminal rendering (xterm.js)
- IndexedDB session persistence (encrypted)

**Security architecture:**

```
Main Thread                          Web Worker (workers/signal.js)
-----------                          ----------------------------
connection_controller.js  <---->     - Non-extractable CryptoKey
  "encrypt this"          postMessage  - Signal sessions (decrypted state)
  "decrypt this"          <-------->   - WASM module
  ciphertext/plaintext only            - All crypto operations
```

The main thread never sees session state - only encrypted envelopes and decrypted messages.

**Key files:**

- `app/javascript/signal/index.js` - Worker proxy (thin wrapper)
- `app/javascript/workers/signal.js` - Crypto isolation (all sensitive ops)
- `app/assets/wasm/` - Custom libsignal WASM bindings
- `app/javascript/controllers/connection_controller.js` - Connection state machine

---

## E2E Encryption Architecture

### Why Signal Protocol?

We use [libsignal](https://github.com/signalapp/libsignal) for E2E encryption because:

- Double Ratchet provides forward secrecy
- Post-quantum security via PQXDH (ML-KEM/Kyber)
- Battle-tested by Signal Messenger
- Pure Rust core compiles to WASM

### Custom WASM Build

**Important:** We built our own WASM wrapper around `libsignal-protocol` (the pure Rust core), NOT `libsignal-client` (which has C dependencies).

See `docs/signal-e2e-encryption.md` for compilation details.

**WASM source:** `libsignal-wasm/` (if exists) or built from libsignal repo

**Browser bindings:**

- `app/javascript/wasm/libsignal_wasm.js` - WASM glue code
- `public/wasm/libsignal_wasm_bg.wasm` - WASM binary

### Key Exchange Flow

```
1. CLI generates PreKeyBundle (identity key, signed prekey, Kyber prekey)
2. CLI displays QR code containing Base32-encoded bundle in URL fragment
3. Browser scans QR → URL: /hubs/{id}#{base32_bundle}
4. Browser parses bundle, creates Signal session (X3DH key agreement)
5. Browser sends encrypted handshake via Action Cable
6. CLI decrypts, creates its Signal session
7. Both sides have Double Ratchet session - server only sees blobs
```

### Bundle Format

The PreKeyBundle is binary (1813 bytes) for QR code efficiency:

- Base32 encoding enables QR alphanumeric mode (smaller QR)
- Bundle in URL fragment (never sent to server)

See `parseBinaryBundle()` in `app/javascript/signal/index.js` for format.

### Session Persistence

**CLI:** Signal state encrypted with AES-256-GCM, key stored in OS keyring.

**Browser:** Sessions encrypted with AES-256-GCM before storing in IndexedDB (`botster_signal` database). All IndexedDB access happens inside the Web Worker. The encryption key is a non-extractable `CryptoKey` that only exists in the worker's memory.

---

## Action Cable Channels

### TerminalRelayChannel

**Purpose:** Relay encrypted messages between browser and CLI.

**Streams:**

- CLI subscribes to: `terminal_relay:{hub_id}:cli`
- Browser subscribes to: `terminal_relay:{hub_id}:browser:{identity}`

**Security:** Server routes encrypted blobs by `recipient_identity`. Cannot decrypt content.

### TunnelChannel (if applicable)

HTTP tunnel for agent dev servers (separate from terminal encryption).

---

## Database Models

### Hub

Represents a running CLI instance.

**Key fields:**

- `id` - Server-assigned ID (used in URLs and subscriptions)
- `identifier` - Local CLI identifier (for config persistence)
- `alive` - Explicit online/offline flag
- `last_seen_at` - Last heartbeat timestamp
- `user_id` - Owner

**Lifecycle:**

- CLI registers on startup → gets/reuses server ID
- CLI sends heartbeats (PUT /hubs/:id) every 30s
- CLI shutdown sets `alive: false` (record preserved for reconnection)
- Active = `alive: true` AND `last_seen_at > 2.minutes.ago`

### Device

Browser device identity for E2E encryption.

**Key fields:**

- `identity_public_key` - Ed25519 public key
- `device_type` - "browser" or "cli"

### Integrations::Github::Message

GitHub webhook events (mentions, cleanup). Delivered to CLI via `github_events:{repo}` ActionCable stream.

### HubCommand

Hub platform commands (browser_wants_preview). Delivered to CLI via `hub_command:{hub_id}` ActionCable stream.

---

## Security Model

### Trust Boundaries

```
┌─────────────────────────────────────────────────────────────┐
│ TRUSTED: User's local machine                               │
│ - CLI with full terminal access                             │
│ - Signal keys in OS keyring                                 │
│ - Git repo and worktrees                                    │
└─────────────────────────────────────────────────────────────┘
                              ↕ E2E encrypted
┌─────────────────────────────────────────────────────────────┐
│ UNTRUSTED: Network / Server                                 │
│ - Rails server (sees only encrypted blobs)                  │
│ - Cannot read terminal content                              │
│ - Cannot forge messages (no keys)                           │
└─────────────────────────────────────────────────────────────┘
                              ↕ E2E encrypted
┌─────────────────────────────────────────────────────────────┐
│ TRUSTED: User's browser                                     │
│ - Signal sessions encrypted with non-extractable CryptoKey  │
│ - Decrypts terminal content locally                         │
└─────────────────────────────────────────────────────────────┘
```

### What the Server Cannot Do

- Read terminal content
- Decrypt any messages
- Access CLI file system
- Impersonate browser or CLI (no keys)
- Store or log encryption bundles

### Browser Security

All sensitive cryptographic operations run in an isolated **Web Worker** (`workers/signal.js`). This provides defense-in-depth against XSS:

**Layer 1: Web Worker Isolation**

The Web Worker has its own global scope, separate from the main thread. Inside the worker:

- Non-extractable AES-256-GCM `CryptoKey` for session encryption
- Decrypted Signal session state (pickled sessions)
- WASM module instance

XSS in the main thread cannot directly access worker variables or memory.

**Layer 2: Non-extractable CryptoKey**

Even within the worker, the wrapping key is non-extractable. `crypto.subtle.exportKey()` will fail.

**Layer 3: Narrow API**

The worker exposes only these operations via `postMessage`:

- `createSession(bundle, hubId)` - returns identity key only
- `encrypt(hubId, message)` - returns ciphertext only
- `decrypt(hubId, envelope)` - returns plaintext only
- `hasSession(hubId)`, `clearSession(hubId)`

Session state never leaves the worker.

**What XSS Can Do:**

- Use the session while the tab is open (send/receive messages)

**What XSS Cannot Do:**

- Export the CryptoKey
- Read decrypted session state
- Steal the session for use elsewhere (on attacker's machine)

### CLI Security

- Signal state encrypted with AES-256-GCM at rest
- Encryption key stored in OS keyring (macOS Keychain, etc.)
- Sessions persist across restarts

---

## WebSocket Reconnection

The CLI WebSocket connection (Action Cable) automatically reconnects with exponential backoff:

- Initial: 1 second
- Max: 30 seconds
- Jitter: random 0-1000ms added

Signal sessions and browser identities persist across reconnections.

---

## Key Design Decisions

### Why Hub IDs vs Identifiers?

- `identifier` - Local UUID, used for CLI config file paths
- `id` - Server-assigned database ID, used in URLs and WebSocket subscriptions

We use server IDs in URLs to guarantee uniqueness across users. The CLI registers its local identifier and receives a server ID back.

### Why Base32 for QR Codes?

QR codes have multiple encoding modes. Alphanumeric mode (A-Z, 0-9, space, $%\*+-./:) is ~40% more efficient than binary mode. Base32 (A-Z, 2-7) fits alphanumeric mode perfectly.

### Why Not Store Bundles Server-Side?

E2E trust model: the server should never see encryption key material. Bundles only exist in:

1. QR code URL fragment (never sent to server)
2. Client memory during session creation

### Why Signal Protocol Over WebRTC DTLS?

- Signal provides forward secrecy (WebRTC DTLS does not)
- Post-quantum hybrid encryption
- Battle-tested implementation
- Works through firewalls/NAT without TURN servers

---

## File Structure

```
cli/
├── src/
│   ├── hub/           # Hub lifecycle, registration
│   ├── agent/         # PTY spawning
│   ├── relay/         # WebSocket + Signal encryption
│   │   ├── connection.rs  # Action Cable with reconnection
│   │   ├── signal.rs      # Signal Protocol manager
│   │   └── signal_stores.rs # Encrypted state persistence
│   └── tui/           # Terminal UI
└── Cargo.toml

app/
├── assets/
│   └── wasm/
│       ├── libsignal_wasm.js      # WASM glue code
│       └── libsignal_wasm_bg.wasm # WASM binary
├── channels/
│   └── terminal_relay_channel.rb  # E2E message relay
├── controllers/
│   └── hubs_controller.rb         # Hub CRUD
├── javascript/
│   ├── signal/
│   │   └── index.js   # Worker proxy (main thread)
│   ├── workers/
│   │   └── signal.js  # Crypto isolation (Web Worker)
│   └── controllers/
│       └── connection_controller.js  # Connection state machine
└── models/
    └── hub.rb

config/initializers/
└── assets.rb  # Asset paths for workers and WASM
```

---

## Changelog

- **2026-01-13**: Moved all crypto to Web Worker for XSS isolation (session state never in main thread)
- **2026-01-13**: Browser sessions now encrypted with non-extractable Web Crypto key
- **2026-01-13**: Added `alive` flag for graceful hub shutdown, WebSocket reconnection with backoff
- **2026-01-13**: Removed legacy `/hubs/:id/bundle` endpoint (server shouldn't store bundles)
- **2026-01-10**: Signal Protocol E2E encryption implemented
