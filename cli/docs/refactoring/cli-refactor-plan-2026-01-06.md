# Botster Hub CLI Refactoring Plan

**Date:** 2026-01-06
**Author:** Claude (Opus 4.5)
**Scope:** `/Users/jasonconigliari/Rails/trybotster/cli/src/`
**Status:** Draft

---

## Executive Summary

The botster CLI has evolved organically and now contains several large modules that violate the single responsibility principle. The primary issues are:

1. **Hub as God Object** (1,769 LOC) - Mixes orchestration, server polling, browser communication, and UI state
2. **Agent doing too much** (1,335 LOC) - PTY management, screen rendering, scrolling, and notifications intertwined
3. **Relay/Connection coupling** (985 LOC) - Encryption, protocol, and WebSocket transport coupled

This plan proposes a phased refactoring following Microsoft Rust guidelines (M-CONCISE-NAMES, M-SMALLER-CRATES, M-REGULAR-FN, etc.) to create a more maintainable, testable architecture.

**Key Outcomes:**
- Hub reduced from 1,769 LOC to ~500 LOC (pure orchestration)
- Clear adapter boundaries (ServerClient, BrowserRelay)
- Independent, testable modules
- Potential for crate extraction

---

## Current State Analysis

### Module Sizes and Responsibilities

| File | LOC | Primary Responsibility | Secondary Concerns |
|------|-----|----------------------|-------------------|
| `hub/mod.rs` | 1,769 | Orchestration | Server polling, browser comm, input conversion, menu handling |
| `agent/mod.rs` | 1,335 | PTY management | Screen rendering, scroll state, notifications |
| `relay/connection.rs` | 985 | WebSocket relay | Encryption, Action Cable protocol, message types |
| `git.rs` | 920 | Worktree management | File copying, teardown scripts |
| `app/input.rs` | 700 | Input handling | All modes in one file |

### Architectural Issues Identified

#### 1. Hub God Object
The Hub struct currently has 29 fields spanning 5 distinct concerns:
- **Core State** (4 fields): state, config, client, device
- **Runtime** (3 fields): tunnel_manager, hub_identifier, tokio_runtime
- **Control Flags** (2 fields): quit, polling_enabled
- **Timing** (2 fields): last_poll, last_heartbeat
- **TUI State** (6 fields): terminal_dims, mode, menu_selected, input_buffer, worktree_selected, connection_url
- **Browser Relay** (6 fields): terminal_output_sender, browser_event_rx, browser_connected, browser_dims, browser_mode, last_*_screen_hash

**Specific violations:**
- `send_browser_output()` and `send_agent_list_to_browser()` should be in a BrowserRelay adapter
- `poll_messages()` and `send_heartbeat()` should be in a ServerClient adapter
- `handle_browser_resize()`, `poll_browser_events()` are browser-specific logic in Hub

#### 2. Agent Overload
The Agent struct mixes:
- PTY session management (cli_pty, server_pty)
- Screen rendering (`get_screen_as_ansi()`)
- Scroll state management (scroll_up, scroll_down, scroll_to_top, scroll_to_bottom)
- Notification handling (notification_rx, poll_notifications)

The `spawn()` method alone is ~163 LOC of dense initialization logic.

#### 3. Relay Connection Coupling
The connection.rs file conflates:
- **Encryption** (RelayState, encrypt, decrypt_command)
- **Protocol** (Action Cable messages, BrowserCommand, BrowserEvent)
- **Transport** (WebSocket connection management)

#### 4. Input Dispatch Monolith
`app/input.rs` handles all input modes in one file with repeated patterns that could be extracted.

---

## Target Architecture

```
cli/src/
  hub/
    mod.rs          # Hub struct (~300 LOC) - pure orchestration
    actions.rs      # HubAction enum (existing, good)
    state.rs        # HubState (existing, good)
    lifecycle.rs    # spawn/close (existing, good)
    polling.rs      # NEW: Server polling logic
    browser.rs      # NEW: Browser event handling

  adapters/
    mod.rs
    server/         # NEW: Server communication
      mod.rs        # ServerClient struct
      types.rs      # Request/response types
    browser/        # NEW: Browser relay adapter
      mod.rs        # BrowserRelay struct
      events.rs     # Event conversion

  agent/
    mod.rs          # Agent struct (~400 LOC)
    pty/
      mod.rs        # PtySession (existing)
      spawn.rs      # NEW: Spawn logic extraction
    screen.rs       # Screen rendering (existing)
    scroll.rs       # NEW: Scroll state machine
    notification.rs # Notification detection (existing)

  relay/
    mod.rs          # Re-exports
    transport.rs    # NEW: WebSocket connection only
    protocol.rs     # NEW: Action Cable protocol
    crypto.rs       # NEW: Encryption layer
    events.rs       # Event types (existing)

  tui/
    mod.rs          # Existing
    input/          # NEW: Split by mode
      mod.rs
      normal.rs
      menu.rs
      worktree.rs
      confirm.rs
```

---

## Phase 1: Foundation (Complexity: Low)

**Duration:** 2-3 hours
**Risk:** Low
**Guidelines:** M-STATIC-VERIFICATION, M-CANONICAL-DOCS, M-MODULE-DOCS, M-APP-ERROR

### 1.1 Enable Comprehensive Clippy Lints

**File:** `cli/Cargo.toml` or `cli/src/lib.rs`

Add to lib.rs:
```rust
#![warn(
    clippy::cargo,
    clippy::complexity,
    clippy::correctness,
    clippy::pedantic,
    clippy::perf,
    clippy::style,
    clippy::suspicious
)]
#![allow(clippy::module_name_repetitions)] // Common in module-per-type patterns
```

**Action:** Run `cargo clippy` and address warnings before proceeding.

### 1.2 Audit and Add Module Documentation

Ensure all public modules have `//!` documentation. Current state shows some modules already compliant (hub/mod.rs is good), but verify:

- [ ] `adapters/mod.rs` (to be created)
- [ ] `agent/scroll.rs` (to be created)
- [ ] `relay/transport.rs` (to be created)

### 1.3 Standardize Error Handling

Already using `anyhow` which is appropriate per M-APP-ERROR. Audit for:
- Inconsistent error propagation
- Missing context in `.context()` calls
- Panics that should be errors (or vice versa per M-PANIC-ON-BUG)

**Deliverables:**
- Clean `cargo clippy` output
- All modules have `//!` documentation
- Consistent error handling patterns

---

## Phase 2: Extract Server Adapter (Complexity: Medium)

**Duration:** 4-6 hours
**Risk:** Medium
**Guidelines:** M-CONCISE-NAMES, M-REGULAR-FN, M-PUBLIC-DEBUG

### 2.1 Create ServerClient Adapter

Extract from `hub/mod.rs`:
- `poll_messages()` (lines 739-858)
- `acknowledge_message()` (lines 861-881)
- `send_heartbeat()` (lines 886-953)
- `send_agent_notification()` (lines 995-1024)

**New file:** `cli/src/hub/polling.rs`

```rust
//! Server polling and heartbeat logic.
//!
//! Handles communication with the Rails server for message polling,
//! heartbeats, and agent notifications.

use std::time::{Duration, Instant};
use reqwest::blocking::Client;
use crate::config::Config;
use crate::hub::HubAction;

/// Configuration for server communication.
pub struct PollingConfig<'a> {
    pub client: &'a Client,
    pub server_url: &'a str,
    pub api_key: &'a str,
    pub poll_interval: u64,
    pub hub_identifier: &'a str,
}

/// Poll the server for messages and return actions.
///
/// # Errors
/// Returns an error if the HTTP request fails.
pub fn poll_messages(
    config: &PollingConfig,
    repo_name: &str,
) -> anyhow::Result<Vec<crate::server::types::MessageData>> {
    // ... extracted logic
}

/// Send heartbeat to register hub with server.
pub fn send_heartbeat(
    config: &PollingConfig,
    repo_name: &str,
    agent_list: &[HeartbeatAgentInfo],
    device_id: Option<&str>,
) -> anyhow::Result<()> {
    // ... extracted logic
}

/// Agent info for heartbeat payload.
#[derive(Debug)]
pub struct HeartbeatAgentInfo {
    pub session_key: String,
    pub last_invocation_url: Option<String>,
}
```

### 2.2 Update Hub to Use Polling Module

Hub methods become thin wrappers:

```rust
impl Hub {
    pub fn tick(&mut self) {
        self.poll_and_handle_messages();
        self.send_heartbeat_if_due();
        self.poll_agent_notifications();
    }

    fn poll_and_handle_messages(&mut self) {
        if !self.should_poll() {
            return;
        }

        let config = polling::PollingConfig {
            client: &self.client,
            server_url: &self.config.server_url,
            // ...
        };

        match polling::poll_messages(&config, &repo_name) {
            Ok(messages) => {
                for msg in messages {
                    // ... convert and handle
                }
            }
            Err(e) => log::warn!("Poll failed: {e}"),
        }
    }
}
```

**Why this approach:**
- M-REGULAR-FN: `poll_messages` is a regular function, not a method
- Easier to unit test (no Hub dependency)
- Clear separation of concerns

**Deliverables:**
- `hub/polling.rs` module with extracted functions
- Hub `poll_messages()` reduced to ~20 LOC wrapper
- Hub `send_heartbeat()` reduced to ~20 LOC wrapper

---

## Phase 3: Extract Browser Adapter (Complexity: Medium-High)

**Duration:** 6-8 hours
**Risk:** Medium
**Guidelines:** M-CONCISE-NAMES, M-REGULAR-FN

### 3.1 Create Browser Communication Module

Extract from `hub/mod.rs`:
- `send_browser_output()` (lines 1623-1653)
- `send_agent_list_to_browser()` (lines 1439-1471)
- `send_worktree_list_to_browser()` (lines 1474-1504)
- `send_agent_selected_to_browser()` (lines 1507-1521)
- `poll_browser_events()` (lines 1278-1401)
- `handle_browser_resize()` (lines 1228-1275)
- `input_action_to_hub_action()` (lines 1403-1434)

**New file:** `cli/src/hub/browser.rs`

```rust
//! Browser event handling and output streaming.
//!
//! This module handles the browser-side of the Hub's communication,
//! converting browser events to Hub actions and streaming terminal
//! output back to connected browsers.

use crate::hub::HubAction;
use crate::relay::{BrowserEvent, TerminalOutputSender, TerminalMessage};

/// Context for browser event handling.
pub struct BrowserContext<'a> {
    pub sender: Option<&'a TerminalOutputSender>,
    pub tokio_runtime: &'a tokio::runtime::Runtime,
    pub browser_connected: bool,
    pub browser_mode: Option<crate::BrowserMode>,
}

/// Handle a browser event and return the corresponding Hub action.
pub fn handle_browser_event(
    event: BrowserEvent,
    ctx: &mut BrowserEventContext,
) -> Option<HubAction> {
    match event {
        BrowserEvent::Input(data) => {
            // ... input parsing logic
        }
        BrowserEvent::Resize(resize) => {
            Some(HubAction::Resize { rows: resize.rows, cols: resize.cols })
        }
        // ...
    }
}

/// Send terminal output to browser.
pub fn send_output(
    sender: &TerminalOutputSender,
    runtime: &tokio::runtime::Runtime,
    output: &str,
) {
    let sender = sender.clone();
    let output = output.to_string();
    runtime.spawn(async move {
        let _ = sender.send(&output).await;
    });
}

/// Build agent list message for browser.
pub fn build_agent_list_message(
    agents: &HashMap<String, Agent>,
    keys_ordered: &[String],
    hub_identifier: &str,
) -> TerminalMessage {
    // ...
}
```

### 3.2 Consolidate Browser State

The Hub has 6 browser-related fields. Group them:

```rust
/// Browser connection state.
pub struct BrowserState {
    pub sender: Option<TerminalOutputSender>,
    pub event_rx: Option<mpsc::Receiver<BrowserEvent>>,
    pub connected: bool,
    pub dims: Option<BrowserResize>,
    pub mode: Option<BrowserMode>,
    pub last_screen_hash: Option<u64>,
}

impl BrowserState {
    pub fn new() -> Self { /* ... */ }

    pub fn is_connected(&self) -> bool {
        self.connected && self.sender.is_some()
    }
}
```

Then Hub becomes:
```rust
pub struct Hub {
    // ...
    pub browser: BrowserState,  // Replaces 6 fields
}
```

**Deliverables:**
- `hub/browser.rs` module
- `BrowserState` struct consolidating browser fields
- Hub browser methods reduced to thin wrappers
- ~400 LOC removed from hub/mod.rs

---

## Phase 4: Split Hub Event Loop (Complexity: Medium)

**Duration:** 4-6 hours
**Risk:** Medium

### 4.1 Extract Run Loop to Separate Function

The `Hub::run()` method (lines 1158-1225) mixes concerns. Extract to:

```rust
// hub/event_loop.rs
//! Hub event loop implementation.

/// Run the hub event loop with TUI.
pub fn run_with_tui(
    hub: &mut Hub,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    shutdown_flag: &AtomicBool,
) -> anyhow::Result<()> {
    // ...
}

/// Single iteration of the event loop (for testing).
pub fn tick_once(
    hub: &mut Hub,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> anyhow::Result<LoopControl> {
    // 1. Poll keyboard input
    // 2. Handle browser events
    // 3. Render
    // 4. Periodic tasks
    Ok(LoopControl::Continue)
}

pub enum LoopControl {
    Continue,
    Quit,
}
```

### 4.2 Simplify handle_action Dispatch

`handle_action()` (lines 230-462) is a large match. Consider grouping:

```rust
impl Hub {
    pub fn handle_action(&mut self, action: HubAction) {
        match &action {
            HubAction::Quit | HubAction::TogglePolling => {
                self.handle_control_action(action);
            }
            HubAction::SelectNext | HubAction::SelectPrevious | ... => {
                self.handle_selection_action(action);
            }
            HubAction::SpawnAgent { .. } | HubAction::CloseAgent { .. } => {
                self.handle_lifecycle_action(action);
            }
            HubAction::OpenMenu | HubAction::CloseModal | ... => {
                self.handle_ui_action(action);
            }
            // ...
        }
    }
}
```

**Deliverables:**
- `hub/event_loop.rs` module
- Hub `handle_action()` split into category handlers
- Clear separation of concerns in event handling

---

## Phase 5: Agent Refactoring (Complexity: High)

**Duration:** 8-10 hours
**Risk:** High (touches core PTY logic)
**Guidelines:** M-CONCISE-NAMES, M-REGULAR-FN

### 5.1 Extract Spawn Logic

The `Agent::spawn()` method (lines 350-513) is 163 LOC. Split into:

**New file:** `cli/src/agent/pty/spawn.rs`

```rust
//! PTY spawn logic for agent sessions.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// Configuration for spawning a PTY.
pub struct PtySpawnConfig {
    pub command: String,
    pub working_dir: PathBuf,
    pub env_vars: HashMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

/// Spawn a new PTY with the given configuration.
///
/// Returns the master PTY, reader, and writer.
///
/// # Errors
/// Returns an error if PTY creation or command spawn fails.
pub fn spawn_pty(
    config: &PtySpawnConfig,
) -> anyhow::Result<(Box<dyn MasterPty>, Box<dyn Read>, Box<dyn Write>, Child)> {
    let pty_system = native_pty_system();
    let size = PtySize {
        rows: config.rows,
        cols: config.cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = pty_system.openpty(size).context("Failed to open PTY")?;

    let mut cmd = build_command(&config.command, &config.working_dir);
    for (key, value) in &config.env_vars {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).context("Failed to spawn command")?;
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok((pair.master, reader, writer, child))
}

/// Build a command from a string.
fn build_command(command_str: &str, working_dir: &Path) -> CommandBuilder {
    let parts: Vec<&str> = command_str.split_whitespace().collect();
    let mut cmd = CommandBuilder::new(parts[0]);
    for arg in &parts[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(working_dir);
    cmd
}

/// Start the PTY reader thread.
pub fn start_reader_thread(
    reader: Box<dyn Read + Send>,
    vt100_parser: Arc<Mutex<Parser>>,
    buffer: Arc<Mutex<VecDeque<String>>>,
    notification_tx: Option<mpsc::Sender<AgentNotification>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        // ... reader loop
    })
}
```

### 5.2 Extract Scroll State

Create a dedicated scroll state machine:

**New file:** `cli/src/agent/scroll.rs`

```rust
//! Scroll state management for terminal buffers.

use std::sync::{Arc, Mutex};
use vt100::Parser;

/// Scroll state for a terminal buffer.
///
/// Wraps a vt100 parser and provides scroll operations.
pub struct ScrollState {
    parser: Arc<Mutex<Parser>>,
}

impl ScrollState {
    /// Create scroll state wrapping an existing parser.
    pub fn new(parser: Arc<Mutex<Parser>>) -> Self {
        Self { parser }
    }

    /// Check if scrolled away from live view.
    pub fn is_scrolled(&self) -> bool {
        let p = self.parser.lock().expect("parser lock poisoned");
        p.screen().scrollback() > 0
    }

    /// Get current scroll offset.
    pub fn offset(&self) -> usize {
        let p = self.parser.lock().expect("parser lock poisoned");
        p.screen().scrollback()
    }

    /// Scroll up by lines.
    pub fn up(&self, lines: usize) {
        let mut p = self.parser.lock().expect("parser lock poisoned");
        let current = p.screen().scrollback();
        p.screen_mut().set_scrollback(current.saturating_add(lines));
    }

    /// Scroll down by lines.
    pub fn down(&self, lines: usize) {
        let mut p = self.parser.lock().expect("parser lock poisoned");
        let current = p.screen().scrollback();
        p.screen_mut().set_scrollback(current.saturating_sub(lines));
    }

    /// Scroll to live view (bottom).
    pub fn to_bottom(&self) {
        let mut p = self.parser.lock().expect("parser lock poisoned");
        p.screen_mut().set_scrollback(0);
    }

    /// Scroll to top of history.
    pub fn to_top(&self) {
        let mut p = self.parser.lock().expect("parser lock poisoned");
        p.screen_mut().set_scrollback(usize::MAX);
    }
}
```

### 5.3 Simplify Agent Struct

After extraction, Agent becomes:

```rust
pub struct Agent {
    // Identity
    pub id: uuid::Uuid,
    pub repo: String,
    pub issue_number: Option<u32>,
    pub branch_name: String,
    pub worktree_path: PathBuf,

    // Lifecycle
    pub start_time: DateTime<Utc>,
    pub status: AgentStatus,
    pub last_invocation_url: Option<String>,

    // PTY sessions
    cli_pty: Option<PtySession>,
    server_pty: Option<PtySession>,
    active_pty: PtyView,

    // Notifications
    notification_rx: Option<Receiver<AgentNotification>>,

    // Tunnel
    pub tunnel_port: Option<u16>,
}

impl Agent {
    // Delegating methods
    pub fn scroll_up(&self, lines: usize) {
        if let Some(scroll) = self.active_scroll_state() {
            scroll.up(lines);
        }
    }

    fn active_scroll_state(&self) -> Option<ScrollState> {
        match self.active_pty {
            PtyView::Cli => self.cli_pty.as_ref().map(|p| ScrollState::new(p.vt100_parser.clone())),
            PtyView::Server => self.server_pty.as_ref().map(|p| ScrollState::new(p.vt100_parser.clone())),
        }
    }
}
```

**Deliverables:**
- `agent/pty/spawn.rs` with PTY spawn logic
- `agent/scroll.rs` with scroll state machine
- Agent struct reduced to ~400 LOC
- Spawn logic testable in isolation

---

## Phase 6: Relay Refactoring (Complexity: Medium-High)

**Duration:** 6-8 hours
**Risk:** Medium
**Guidelines:** M-CONCISE-NAMES, M-PUBLIC-DEBUG

### 6.1 Split Relay Connection

Current: `relay/connection.rs` (985 LOC)

Split into:

**`relay/crypto.rs`** - Encryption layer
```rust
//! E2E encryption for terminal relay.
//!
//! Uses crypto_box (TweetNaCl compatible) for encryption.

use crypto_box::{aead::Aead, PublicKey, SalsaBox, SecretKey};

/// Encryption state for a browser connection.
pub struct CryptoState {
    secret_key: SecretKey,
    shared_box: Option<SalsaBox>,
}

impl CryptoState {
    pub fn new(secret_key: SecretKey) -> Self { /* ... */ }

    /// Set peer public key and compute shared secret.
    pub fn set_peer_key(&mut self, peer_public_key_base64: &str) -> anyhow::Result<()> { /* ... */ }

    /// Encrypt a message.
    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<EncryptedEnvelope> { /* ... */ }

    /// Decrypt a message.
    pub fn decrypt(&self, envelope: &EncryptedEnvelope) -> anyhow::Result<Vec<u8>> { /* ... */ }

    pub fn is_ready(&self) -> bool { /* ... */ }
}
```

**`relay/protocol.rs`** - Action Cable protocol
```rust
//! Action Cable protocol handling.
//!
//! Implements the Action Cable wire protocol for WebSocket communication.

/// Action Cable message wrapper.
#[derive(Debug, Serialize, Deserialize)]
pub struct CableMessage {
    pub command: String,
    pub identifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Channel identifier for subscription.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelIdentifier {
    pub channel: String,
    pub hub_identifier: String,
    pub device_type: String,
}

/// Build a subscribe message.
pub fn subscribe_message(identifier: &ChannelIdentifier) -> CableMessage { /* ... */ }

/// Build a relay message with encrypted data.
pub fn relay_message(identifier: &str, envelope: &EncryptedEnvelope) -> CableMessage { /* ... */ }

/// Parse an incoming cable message.
pub fn parse_incoming(text: &str) -> Option<IncomingMessage> { /* ... */ }
```

**`relay/transport.rs`** - WebSocket connection
```rust
//! WebSocket transport for terminal relay.
//!
//! Handles the raw WebSocket connection lifecycle.

use tokio_tungstenite::{connect_async, tungstenite::Message};

/// WebSocket connection to Action Cable.
pub struct RelayTransport {
    write: Arc<Mutex<SplitSink<...>>>,
    read_handle: JoinHandle<()>,
}

impl RelayTransport {
    /// Connect to Action Cable WebSocket.
    pub async fn connect(url: &str, origin: &str) -> anyhow::Result<Self> { /* ... */ }

    /// Send a message.
    pub async fn send(&self, msg: Message) -> anyhow::Result<()> { /* ... */ }

    /// Subscribe to incoming messages.
    pub fn subscribe(&self) -> mpsc::Receiver<Message> { /* ... */ }
}
```

**`relay/connection.rs`** - Coordinator (much smaller)
```rust
//! Terminal relay coordinator.
//!
//! Coordinates crypto, protocol, and transport for browser communication.

use super::{crypto::CryptoState, protocol, transport::RelayTransport};

pub struct TerminalRelay {
    crypto: CryptoState,
    transport: Option<RelayTransport>,
    hub_identifier: String,
    // ...
}
```

**Deliverables:**
- `relay/crypto.rs` (~150 LOC)
- `relay/protocol.rs` (~100 LOC)
- `relay/transport.rs` (~150 LOC)
- `relay/connection.rs` reduced to ~200 LOC coordinator

---

## Phase 7: Input Refactoring (Complexity: Low)

**Duration:** 2-3 hours
**Risk:** Low

### 7.1 Split Input by Mode

Current: `app/input.rs` (700 LOC) handles all modes.

Split into:

```
tui/input/
  mod.rs          # Re-exports, dispatch function
  normal.rs       # Normal mode input
  menu.rs         # Menu mode input
  worktree.rs     # Worktree selection input
  confirm.rs      # Confirmation dialogs
  connection.rs   # Connection code mode
```

Each file ~80-120 LOC, single responsibility.

**`tui/input/mod.rs`:**
```rust
//! Input handling for TUI modes.
//!
//! Each mode has its own handler module.

mod normal;
mod menu;
mod worktree;
mod confirm;
mod connection;

pub use normal::handle_normal_key;
pub use menu::handle_menu_key;
// ...

/// Dispatch input to appropriate mode handler.
pub fn dispatch(mode: &AppMode, key: KeyCode, mods: KeyModifiers, ctx: &InputContext) -> InputAction {
    match mode {
        AppMode::Normal => normal::handle_normal_key(key, mods, ctx.terminal_rows),
        AppMode::Menu => menu::handle_menu_key(key, ctx.menu_selected, ctx.menu_count),
        // ...
    }
}
```

**Deliverables:**
- 5 small mode-specific input modules
- Cleaner dispatch logic
- Easier to add new modes

---

## Phase 8: Consider Crate Splitting (Complexity: High)

**Duration:** Evaluation only (1-2 hours)
**Risk:** Low (evaluation phase)
**Guidelines:** M-SMALLER-CRATES

After the previous phases, evaluate whether to split into crates:

### Candidates for Extraction

| Candidate Crate | Contents | Dependencies |
|-----------------|----------|--------------|
| `botster-agent` | Agent, PtySession, ScrollState | vt100, portable-pty |
| `botster-relay` | TerminalRelay, crypto, protocol | crypto_box, tokio-tungstenite |
| `botster-git` | WorktreeManager | git2, globset |
| `botster-core` | Hub, HubAction, HubState | botster-agent, botster-relay |

### Decision Criteria

**Extract if:**
- Module has minimal dependencies on Hub
- Module could be reused independently
- Module has stable interface
- Testing would benefit from isolation

**Keep together if:**
- Heavy cross-module dependencies
- Frequent interface changes expected
- Compilation time not a concern

### Recommendation

For now, **defer crate splitting** until:
1. Phases 1-7 are complete
2. Module boundaries have stabilized
3. Team has bandwidth for workspace maintenance

The internal modularization provides most benefits without the overhead of crate management.

---

## Risk Assessment and Mitigation

### High-Risk Areas

| Area | Risk | Mitigation |
|------|------|------------|
| PTY spawn refactoring | Breaking existing agent spawn | Comprehensive test coverage before changes |
| Browser event handling | Breaking browser connectivity | Manual testing with browser client |
| Encryption changes | Breaking E2E security | Unit tests for encrypt/decrypt roundtrip |

### Rollback Strategy

Each phase should be merged independently. If issues arise:
1. Revert the specific phase commit
2. Investigate in isolation
3. Re-implement with fixes

### Testing Strategy

1. **Before each phase:** Run full test suite, ensure passing
2. **During refactoring:** Add tests for extracted functions
3. **After each phase:** Run integration tests with actual GitHub webhook

Key tests to add:
- `polling::poll_messages()` with mocked HTTP client
- `browser::handle_browser_event()` unit tests
- `spawn_pty()` with real PTY (integration test)
- `CryptoState` encrypt/decrypt roundtrip

---

## Success Metrics

| Metric | Current | Target | Notes |
|--------|---------|--------|-------|
| `hub/mod.rs` LOC | 1,769 | ~500 | Pure orchestration |
| `agent/mod.rs` LOC | 1,335 | ~400 | Core agent logic only |
| `relay/connection.rs` LOC | 985 | ~200 | Coordinator only |
| Max function LOC | 163 (spawn) | <50 | Per M-REGULAR-FN spirit |
| Test coverage | Unknown | >60% | For refactored modules |
| Clippy warnings | Unknown | 0 | Per M-STATIC-VERIFICATION |

---

## Implementation Order

```
Phase 1 (Foundation)      [Day 1]
    |
    v
Phase 2 (Server Adapter)  [Day 1-2]
    |
    v
Phase 3 (Browser Adapter) [Day 2-3]
    |
    v
Phase 4 (Hub Event Loop)  [Day 3]
    |
    v
Phase 5 (Agent)           [Day 4-5]
    |
    v
Phase 6 (Relay)           [Day 5-6]
    |
    v
Phase 7 (Input)           [Day 6]
    |
    v
Phase 8 (Evaluation)      [Day 7]
```

**Total Estimated Time:** 5-7 working days

---

## Appendix A: MS Rust Guidelines Reference

| Guideline | Application |
|-----------|-------------|
| M-CONCISE-NAMES | Avoid `Manager`, `Service`; use specific names |
| M-SMALLER-CRATES | Consider crate extraction for independent modules |
| M-REGULAR-FN | Prefer free functions over associated functions |
| M-STATIC-VERIFICATION | Enable comprehensive clippy lints |
| M-CANONICAL-DOCS | All public modules need `//!` documentation |
| M-MODULE-DOCS | Functions need summary < 15 words |
| M-APP-ERROR | Use anyhow for application errors |
| M-PUBLIC-DEBUG | All public types implement Debug |
| M-PUBLIC-DISPLAY | Types meant to be read implement Display |
| M-PANIC-ON-BUG | Programming bugs should panic |
| M-LOG-STRUCTURED | Use structured logging with dot-notation |

---

## Appendix B: File Change Summary

### Files to Create
- `cli/src/hub/polling.rs`
- `cli/src/hub/browser.rs`
- `cli/src/hub/event_loop.rs`
- `cli/src/agent/pty/spawn.rs`
- `cli/src/agent/scroll.rs`
- `cli/src/relay/crypto.rs`
- `cli/src/relay/protocol.rs`
- `cli/src/relay/transport.rs`
- `cli/src/tui/input/mod.rs`
- `cli/src/tui/input/normal.rs`
- `cli/src/tui/input/menu.rs`
- `cli/src/tui/input/worktree.rs`
- `cli/src/tui/input/confirm.rs`
- `cli/src/tui/input/connection.rs`

### Files to Modify
- `cli/src/hub/mod.rs` (major reduction)
- `cli/src/agent/mod.rs` (moderate reduction)
- `cli/src/relay/connection.rs` (major reduction)
- `cli/src/relay/mod.rs` (add re-exports)
- `cli/src/app/input.rs` (may deprecate or redirect)

### Files Unchanged
- `cli/src/hub/state.rs` (already well-structured)
- `cli/src/hub/actions.rs` (already well-structured)
- `cli/src/hub/lifecycle.rs` (already well-structured)
- `cli/src/server/messages.rs` (already well-structured)
- `cli/src/agent/notification.rs` (already focused)
- `cli/src/agent/screen.rs` (already focused)

---

## Appendix C: git.rs Analysis

The `git.rs` file (920 LOC) is cohesive and focused on worktree management. The main complexity is in `copy_matching_files()` which has deep nesting.

**Recommendation:** Leave git.rs as-is for now, but consider extracting `copy_matching_files` into a helper if Phase 5-7 complete early. This is lower priority than the Hub/Agent/Relay refactoring.

Potential future improvement:
```rust
// git/copy.rs
pub fn copy_matching_files(
    source_root: &Path,
    dest_root: &Path,
    globset: &GlobSet,
) -> anyhow::Result<CopyReport> {
    // ... flattened logic with early returns
}
```

---

*End of Refactoring Plan*
