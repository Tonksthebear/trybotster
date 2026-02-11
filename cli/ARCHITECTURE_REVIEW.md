# Botster Hub CLI Architecture Review and Refactoring Plan

**Date**: January 2026
**Status**: Proposed
**Author**: Architecture Review

---

## Executive Summary

The Botster Hub CLI is a powerful terminal-based application for managing AI coding agents via GitHub integration. However, the codebase has grown organically and now suffers from a **god object anti-pattern** in `main.rs` (3,539 lines). This document provides a comprehensive analysis of the current architecture, identifies violations of Rust best practices, and proposes a phased refactoring plan to improve maintainability, testability, and AI-assisted development.

---

## Table of Contents

1. [Current State Analysis](#current-state-analysis)
2. [Problems Identified](#problems-identified)
3. [Guideline Violations](#guideline-violations)
4. [Proposed Module Structure](#proposed-module-structure)
5. [Implementation Plan](#implementation-plan)
6. [Risk Assessment](#risk-assessment)
7. [Success Metrics](#success-metrics)
8. [Appendix: Code Inventory](#appendix-code-inventory)

---

## Current State Analysis

### File Statistics

| File | Lines | Purpose | Complexity |
|------|-------|---------|------------|
| `main.rs` | 3,539 | **GOD OBJECT** - Everything | Critical |
| `agent.rs` | 2,089 | Agent/PTY session handling | High |
| `webrtc_handler.rs` | 993 | WebRTC P2P browser connections | Medium |
| `git.rs` | 939 | Git worktree management | Medium |
| `tunnel.rs` | 474 | HTTP tunnel forwarding | Low |
| `terminal_widget.rs` | 356 | Terminal rendering widget | Low |
| `render.rs` | 231 | Terminal rendering helpers | Low |
| `terminal.rs` | 150 | External terminal spawning | Low |
| `config.rs` | 127 | Configuration loading | Low |
| `prompt.rs` | 98 | Prompt management | Low |
| `lib.rs` | 24 | Module exports | Trivial |
| `app.rs` | 24 | Nearly empty (unused) | Trivial |
| **Total** | **9,044** | | |

### BotsterApp Struct Analysis

The `BotsterApp` struct in `main.rs` currently has **17 fields**, each representing a distinct responsibility:

```rust
struct BotsterApp {
    // Agent Management (4 fields)
    agents: HashMap<String, Agent>,
    agent_keys_ordered: Vec<String>,
    selected: usize,
    last_agent_screen_hash: HashMap<String, u64>,

    // Configuration (2 fields)
    config: Config,
    git_manager: WorktreeManager,

    // Network/Server Communication (3 fields)
    client: Client,
    last_poll: Instant,
    last_heartbeat: Instant,

    // UI State (6 fields)
    terminal_rows: u16,
    terminal_cols: u16,
    mode: AppMode,
    menu_selected: usize,
    input_buffer: String,
    available_worktrees: Vec<(String, String)>,
    worktree_selected: usize,
    polling_enabled: bool,
    quit: bool,

    // WebRTC/Async Runtime (2 fields)
    tokio_runtime: tokio::runtime::Runtime,
    webrtc_handler: Arc<StdMutex<WebRTCHandler>>,

    // Infrastructure (2 fields)
    hub_identifier: String,
    tunnel_manager: Arc<TunnelManager>,
}
```

### Responsibility Breakdown

`main.rs` contains the following distinct responsibilities:

1. **CLI Definition** (~100 lines)
   - `Cli` struct with clap derive
   - `Commands` enum with 11 subcommands

2. **Application State** (~100 lines)
   - `BotsterApp` struct
   - `AppMode` enum
   - `AgentSpawnConfig` struct

3. **Event Handling** (~400 lines)
   - `handle_events()` - Polls crossterm events
   - `handle_mouse_event()` - Mouse scroll handling
   - `handle_key_event()` - Keyboard dispatch
   - `handle_normal_mode_key()` - Normal mode keys
   - `handle_menu_mode_key()` - Menu navigation
   - `handle_worktree_select_key()` - Worktree selection
   - `handle_create_worktree_key()` - Worktree creation input
   - `handle_prompt_input_key()` - Prompt input
   - `handle_close_confirm_key()` - Close confirmation

4. **UI Rendering** (~300 lines)
   - `view()` - Main render function
   - `centered_rect()` - Layout helper
   - `buffer_to_ansi()` - ANSI escape sequence generation
   - `convert_browser_key_to_crossterm()` - Key conversion

5. **Server Communication** (~400 lines)
   - `poll_messages()` - Polls Rails server for new work
   - `acknowledge_message()` - Sends acknowledgment to Rails
   - `send_heartbeat()` - Registers hub with server
   - `send_agent_notification()` - Sends notifications to Rails
   - `poll_agent_notifications()` - Collects terminal notifications

6. **Agent Management** (~600 lines)
   - `spawn_agent_with_config()` - Core spawning logic
   - `spawn_agent_from_worktree()` - Spawns from existing worktree
   - `create_and_spawn_agent()` - Creates new worktree and spawns
   - `spawn_agent_for_message()` - Spawns from server message
   - `close_agent()` - Closes agent cleanly
   - `load_available_worktrees()` - Lists available worktrees
   - `build_web_agent_list()` - Builds agent info for WebRTC

7. **WebRTC Handling** (~100 lines)
   - `handle_webrtc_offer()` - Processes WebRTC offers

8. **Cleanup & Process Management** (~200 lines)
   - `handle_cleanup_message()` - Handles issue/PR closure
   - `kill_orphaned_processes()` - Kills orphaned claude processes
   - `get_parent_pid()` - Gets parent PID for orphan detection

9. **CLI Subcommands** (~300 lines)
   - `json_get()`, `json_set()`, `json_delete()` - JSON manipulation
   - `get_prompt()` - Gets agent prompt
   - `delete_worktree()` - Deletes a worktree
   - `list_worktrees()` - Lists all worktrees
   - `check_for_updates()` - Checks for new versions
   - `update_binary()` - Self-updates the binary

10. **Main Loop** (~600 lines)
    - `run_interactive()` - Main TUI event loop
    - `run_headless()` - Headless mode (stub)
    - `main()` - Entry point
    - Signal handling, terminal setup/teardown

---

## Problems Identified

### 1. God Object Anti-Pattern (Critical)

`main.rs` violates the Single Responsibility Principle catastrophically:

- **3,539 lines** in a single file
- **17+ fields** on `BotsterApp`
- **40+ methods** with mixed concerns
- No clear separation between UI, networking, and business logic

**Impact**:
- Compile time increases with every change
- Cognitive load for developers is extreme
- AI tools struggle to understand the full context
- Testing is nearly impossible without full integration tests

### 2. Mixed Async and Sync Code

The codebase awkwardly mixes synchronous and asynchronous code:

```rust
// Blocking async calls scattered throughout
app.tokio_runtime.block_on(async move {
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        async { ... }
    ).await;
});
```

This pattern appears **30+ times** in `main.rs`, creating:
- Potential deadlocks
- Hard-to-debug timing issues
- Poor separation of concerns

### 3. State Management Complexity

UI state is mixed with business logic:

```rust
// UI state
mode: AppMode,
menu_selected: usize,
input_buffer: String,

// Business logic state
agents: HashMap<String, Agent>,
last_poll: Instant,
```

This mixing makes it impossible to:
- Unit test UI logic separately
- Reason about state transitions
- Implement undo/redo or state persistence

### 4. Duplicate Code

Several patterns are repeated:

**WebRTC timeout pattern** (appears 20+ times):
```rust
app.tokio_runtime.block_on(async move {
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        async {
            let handler = webrtc_handler.lock().unwrap();
            // ... operation ...
        }
    ).await;
});
```

**Agent label formatting** (appears 6+ times):
```rust
let label = if let Some(num) = agent.issue_number {
    format!("issue #{}", num)
} else {
    format!("branch {}", agent.branch_name)
};
```

### 5. Magic Numbers

Undocumented constants scattered throughout:

```rust
Duration::from_millis(50)   // WebRTC timeout - why 50ms?
Duration::from_secs(10)     // HTTP client timeout
Duration::from_secs(30)     // Heartbeat interval
Duration::from_millis(16)   // Main loop sleep (60 FPS)
const MAX_BUFFER_LINES: usize = 20000;  // In agent.rs
```

### 6. Error Handling Inconsistencies

Mixed error handling strategies:

```rust
// Pattern 1: Silent failure
if let Err(e) = ... { log::warn!(...); }

// Pattern 2: Bail on error
anyhow::bail!("...");

// Pattern 3: Context chain
.with_context(|| format!("..."))?;

// Pattern 4: Unwrap with default
.unwrap_or_default()
```

---

## Guideline Violations

Based on Microsoft's Rust API Guidelines and general best practices:

### M-SMALLER-CRATES: "If in doubt, split the crate"

**Violation**: `main.rs` contains 3,539 lines of mixed concerns.

**Recommendation**: Split into at least 8 modules:
- `app/mod.rs` - Application core
- `app/events.rs` - Event handling
- `app/ui.rs` - UI rendering
- `server/mod.rs` - Server communication
- `agents/mod.rs` - Agent management
- `commands.rs` - CLI subcommands
- `update.rs` - Self-update logic
- `notifications.rs` - Notification handling

### M-CONCISE-NAMES: Avoid weasel words

**Violation**: Some names could be clearer:

```rust
// Current
fn spawn_agent_for_message(...)  // What kind of message?
fn handle_cleanup_message(...)   // Cleanup of what?

// Better
fn spawn_agent_from_server_task(...)
fn handle_issue_closed_cleanup(...)
```

### M-DOCUMENTED-MAGIC: Document magic values

**Violation**: Many magic numbers lack documentation:

```rust
// Should be:
/// WebRTC operation timeout. 50ms allows for network latency
/// while preventing UI blocking. Tune if connection quality varies.
const WEBRTC_OPERATION_TIMEOUT_MS: u64 = 50;

/// Main loop frame time for ~60 FPS UI refresh.
const FRAME_TIME_MS: u64 = 16;
```

### M-PUBLIC-DEBUG: Public types should implement Debug

**Violation**: `AgentSpawnConfig` and `AppMode` lack `#[derive(Debug)]`:

```rust
// Current
#[derive(Clone, PartialEq)]
enum AppMode { ... }

// Should be
#[derive(Clone, Debug, PartialEq)]
enum AppMode { ... }
```

### M-DESIGN-FOR-AI: Structure for AI comprehension

**Violation**: Files exceed 500 lines, making AI tools less effective.

**From the guideline**:
> Keep files under 500 lines. AI coding assistants have limited context windows, and smaller, focused modules are easier to understand and modify.

---

## Proposed Module Structure

### New File Structure

```
cli/src/
|-- main.rs              (~200 lines - entry point only)
|-- lib.rs               (module exports)
|-- constants.rs         (documented magic numbers)
|
|-- app/
|   |-- mod.rs           (BotsterApp core, AppMode, AppState)
|   |-- events.rs        (all event handling)
|   |-- ui.rs            (view rendering)
|   +-- state.rs         (UI state management)
|
|-- server/
|   |-- mod.rs           (ServerClient, polling)
|   |-- messages.rs      (message types, parsing)
|   +-- heartbeat.rs     (heartbeat logic)
|
|-- agents/
|   |-- mod.rs           (agent spawning, lifecycle)
|   |-- spawn.rs         (spawn configuration, creation)
|   +-- process.rs       (orphan killing, process mgmt)
|
|-- commands/
|   |-- mod.rs           (CLI subcommand dispatch)
|   |-- json.rs          (json_get, json_set, json_delete)
|   |-- worktree.rs      (delete_worktree, list_worktrees)
|   +-- prompt.rs        (get_prompt)
|
|-- update.rs            (self-update logic)
|-- notifications.rs     (agent notification handling)
|
|-- (existing files - unchanged)
|-- agent.rs             (PTY/Agent session)
|-- config.rs            (configuration)
|-- git.rs               (worktree management)
|-- prompt.rs            (prompt management)
|-- render.rs            (terminal rendering)
|-- terminal.rs          (external terminal)
|-- terminal_widget.rs   (terminal widget)
|-- tunnel.rs            (HTTP tunnel)
+-- webrtc_handler.rs    (WebRTC P2P)
```

### Module Responsibilities

#### `main.rs` (~200 lines)

```rust
// Entry point only
mod app;
mod server;
mod agents;
mod commands;
mod update;
mod notifications;
mod constants;

fn main() -> Result<()> {
    setup_logging();
    setup_panic_hook();

    match Cli::parse().command {
        Commands::Start { headless } => {
            if headless {
                app::run_headless()
            } else {
                app::run_interactive()
            }
        }
        Commands::JsonGet { file, key } => commands::json::get(&file, &key),
        Commands::Update { check } => update::handle(check),
        // ... other commands
    }
}
```

#### `app/mod.rs` (~300 lines)

```rust
pub mod events;
pub mod ui;
pub mod state;

/// Core application state with reduced responsibilities
pub struct BotsterApp {
    state: AppState,
    agents: AgentManager,
    server: ServerClient,
    webrtc: WebRtcManager,
    tunnel: TunnelManager,
}

impl BotsterApp {
    pub fn new(terminal_rows: u16, terminal_cols: u16) -> Result<Self>;
    pub fn run(&mut self) -> Result<()>;
    fn tick(&mut self) -> Result<()>;
}

pub fn run_interactive() -> Result<()>;
pub fn run_headless() -> Result<()>;
```

#### `app/events.rs` (~400 lines)

```rust
impl BotsterApp {
    pub fn handle_events(&mut self) -> Result<bool>;
    pub fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<bool>;
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<bool>;

    fn handle_normal_mode_key(&mut self, key: KeyEvent) -> Result<bool>;
    fn handle_menu_mode_key(&mut self, key: KeyEvent) -> Result<bool>;
    // ... other mode handlers
}

/// Convert browser key input to crossterm KeyEvent
pub fn browser_key_to_crossterm(input: &KeyInput) -> Option<KeyEvent>;
```

#### `app/ui.rs` (~300 lines)

```rust
impl BotsterApp {
    /// Render the TUI and return ANSI output for WebRTC streaming
    pub fn view(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        browser_dims: Option<BrowserDimensions>,
    ) -> Result<(String, u16, u16)>;
}

/// Create a centered rectangle within a parent rect
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect;

/// Convert a ratatui Buffer to ANSI escape sequences
pub fn buffer_to_ansi(
    buffer: &Buffer,
    width: u16,
    height: u16,
    browser_dims: Option<BrowserDimensions>,
) -> String;
```

#### `server/mod.rs` (~300 lines)

```rust
pub struct ServerClient {
    client: Client,
    config: Config,
    hub_identifier: String,
    last_poll: Instant,
    last_heartbeat: Instant,
}

impl ServerClient {
    pub fn new(config: &Config, hub_identifier: &str) -> Result<Self>;
    pub fn poll_messages(&mut self) -> Result<Vec<ServerMessage>>;
    pub fn acknowledge_message(&self, message_id: i64) -> Result<()>;
    pub fn send_heartbeat(&mut self, agents: &[AgentInfo]) -> Result<()>;
    pub fn send_notification(&self, notification: &AgentNotification) -> Result<()>;
}
```

#### `agents/mod.rs` (~400 lines)

```rust
pub mod spawn;
pub mod process;

pub struct AgentManager {
    agents: HashMap<String, Agent>,
    ordered_keys: Vec<String>,
    selected: usize,
    screen_hashes: HashMap<String, u64>,
}

impl AgentManager {
    pub fn spawn(&mut self, config: spawn::AgentSpawnConfig) -> Result<String>;
    pub fn close(&mut self, session_key: &str, delete_worktree: bool) -> Result<()>;
    pub fn get_selected(&self) -> Option<&Agent>;
    pub fn select_next(&mut self);
    pub fn select_prev(&mut self);
    pub fn resize_all(&mut self, rows: u16, cols: u16);
}
```

#### `constants.rs` (~50 lines)

```rust
//! Documented constants used throughout the application

/// WebRTC operation timeout in milliseconds.
/// Set to 50ms to allow for network latency while preventing UI blocking.
pub const WEBRTC_TIMEOUT_MS: u64 = 50;

/// HTTP client request timeout in seconds.
pub const HTTP_TIMEOUT_SECS: u64 = 10;

/// Heartbeat interval in seconds.
/// Hub sends heartbeat to register with Rails server.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Main loop frame time for ~60 FPS UI refresh.
pub const FRAME_TIME_MS: u64 = 16;

/// Maximum lines in terminal scrollback buffer.
pub const MAX_SCROLLBACK_LINES: usize = 20_000;

/// Terminal widget width as percentage of total width.
pub const TERMINAL_WIDTH_PERCENT: u16 = 70;
```

---

## Implementation Plan

### Phase 1: Extract CLI Commands (Low Risk)

**Effort**: 2-4 hours
**Risk**: Low - No changes to core application logic

**Tasks**:
1. Create `commands/mod.rs`, `commands/json.rs`, `commands/worktree.rs`
2. Move `json_get()`, `json_set()`, `json_delete()` to `commands/json.rs`
3. Move `delete_worktree()`, `list_worktrees()` to `commands/worktree.rs`
4. Move `get_prompt()` to `commands/prompt.rs`
5. Update `main.rs` to use new module paths

**Files Changed**: 4 new files, 1 modified
**Lines Removed from main.rs**: ~300

### Phase 2: Extract Update Logic (Low Risk)

**Effort**: 1-2 hours
**Risk**: Low - Self-contained functionality

**Tasks**:
1. Create `update.rs`
2. Move `check_for_updates()` and `update_binary()` to `update.rs`
3. Update `main.rs` imports

**Files Changed**: 1 new file, 1 modified
**Lines Removed from main.rs**: ~200

### Phase 3: Create Constants Module (Low Risk)

**Effort**: 1-2 hours
**Risk**: Low - Improves documentation without behavior change

**Tasks**:
1. Create `constants.rs`
2. Define documented constants
3. Replace magic numbers throughout codebase
4. Add doc comments explaining each value

**Files Changed**: 1 new file, ~10 modified
**Lines Removed from main.rs**: ~10 (replaced with imports)

### Phase 4: Extract Notifications (Low-Medium Risk)

**Effort**: 2-4 hours
**Risk**: Low-Medium - Touches server communication

**Tasks**:
1. Create `notifications.rs`
2. Move `poll_agent_notifications()` and `send_agent_notification()`
3. Define notification types/structs
4. Update `BotsterApp` to use notification module

**Files Changed**: 1 new file, 1 modified
**Lines Removed from main.rs**: ~100

### Phase 5: Create App Module Structure (Medium Risk)

**Effort**: 4-8 hours
**Risk**: Medium - Core refactoring

**Tasks**:
1. Create `app/mod.rs`, `app/state.rs`
2. Define `AppState` struct for UI-only state
3. Create `app/ui.rs` with rendering functions
4. Create `app/events.rs` with event handlers
5. Refactor `BotsterApp` to use new structure
6. Move `run_interactive()` and `run_headless()` to `app/mod.rs`

**Files Changed**: 4 new files, 1 heavily modified
**Lines Removed from main.rs**: ~1,200

### Phase 6: Extract Server Communication (Medium Risk)

**Effort**: 4-6 hours
**Risk**: Medium - Touches critical networking code

**Tasks**:
1. Create `server/mod.rs`
2. Define `ServerClient` struct
3. Move `poll_messages()`, `acknowledge_message()`, `send_heartbeat()`
4. Create `server/messages.rs` for message types
5. Update `BotsterApp` to use `ServerClient`

**Files Changed**: 2 new files, 2 modified
**Lines Removed from main.rs**: ~400

### Phase 7: Extract Agent Management (Higher Risk)

**Effort**: 6-10 hours
**Risk**: Higher - Core functionality

**Tasks**:
1. Create `agents/mod.rs`
2. Create `agents/spawn.rs` with `AgentSpawnConfig` and spawning logic
3. Create `agents/process.rs` with orphan killing logic
4. Define `AgentManager` facade
5. Move all agent-related methods
6. Extensive testing required

**Files Changed**: 3 new files, 2 heavily modified
**Lines Removed from main.rs**: ~800

### Phase 8: Clean Up and Polish (Low Risk)

**Effort**: 2-4 hours
**Risk**: Low - Final cleanup

**Tasks**:
1. Add `#[derive(Debug)]` to all public types
2. Add module-level documentation
3. Update `lib.rs` exports
4. Remove dead code
5. Run clippy and fix warnings
6. Update existing documentation

---

## Risk Assessment

### Low Risk Changes

| Phase | Change | Risk Mitigation |
|-------|--------|-----------------|
| 1 | CLI commands extraction | No runtime behavior change |
| 2 | Update logic extraction | Self-contained, easily tested |
| 3 | Constants module | Find-and-replace only |
| 4 | Notifications extraction | Simple data flow |

### Medium Risk Changes

| Phase | Change | Risk Mitigation |
|-------|--------|-----------------|
| 5 | App module structure | Incremental moves, regression tests |
| 6 | Server communication | Mock server for testing |

### Higher Risk Changes

| Phase | Change | Risk Mitigation |
|-------|--------|-----------------|
| 7 | Agent management | Comprehensive integration tests |

### Rollback Strategy

Each phase should be:
1. Implemented in a feature branch
2. Fully tested before merge
3. Deployable independently
4. Revertible without affecting other phases

---

## Success Metrics

### Quantitative Targets

| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| `main.rs` lines | 3,539 | <300 | 91% reduction |
| Largest file | 3,539 | <500 | Per AI guideline |
| `BotsterApp` fields | 17 | <8 | 53% reduction |
| Test coverage | ~5% | >50% | 10x improvement |
| Compile time (incremental) | ~15s | <5s | 3x faster |

### Qualitative Improvements

1. **Maintainability**: Each module has a single responsibility
2. **Testability**: Components can be unit tested in isolation
3. **AI-Friendliness**: Files fit within context windows
4. **Onboarding**: New developers can understand modules independently
5. **Compile Time**: Changes to one module don't recompile everything

---

## Appendix: Code Inventory

### Functions in main.rs (Current)

| Function | Lines | Category | Target Module |
|----------|-------|----------|---------------|
| `centered_rect()` | 25 | UI | `app/ui.rs` |
| `buffer_to_ansi()` | 130 | UI | `app/ui.rs` |
| `convert_browser_key_to_crossterm()` | 65 | Events | `app/events.rs` |
| `BotsterApp::new()` | 60 | Core | `app/mod.rs` |
| `BotsterApp::spawn_agent_with_config()` | 130 | Agents | `agents/spawn.rs` |
| `BotsterApp::handle_events()` | 30 | Events | `app/events.rs` |
| `BotsterApp::handle_mouse_event()` | 25 | Events | `app/events.rs` |
| `BotsterApp::handle_key_event()` | 15 | Events | `app/events.rs` |
| `BotsterApp::handle_normal_mode_key()` | 130 | Events | `app/events.rs` |
| `BotsterApp::handle_menu_mode_key()` | 55 | Events | `app/events.rs` |
| `BotsterApp::handle_worktree_select_key()` | 35 | Events | `app/events.rs` |
| `BotsterApp::handle_create_worktree_key()` | 30 | Events | `app/events.rs` |
| `BotsterApp::handle_prompt_input_key()` | 30 | Events | `app/events.rs` |
| `BotsterApp::handle_close_confirm_key()` | 70 | Events | `app/events.rs` |
| `BotsterApp::load_available_worktrees()` | 70 | Agents | `agents/mod.rs` |
| `BotsterApp::spawn_agent_from_worktree()` | 40 | Agents | `agents/spawn.rs` |
| `BotsterApp::create_and_spawn_agent()` | 40 | Agents | `agents/spawn.rs` |
| `BotsterApp::view()` | 280 | UI | `app/ui.rs` |
| `BotsterApp::poll_messages()` | 100 | Server | `server/mod.rs` |
| `BotsterApp::acknowledge_message()` | 20 | Server | `server/mod.rs` |
| `BotsterApp::poll_agent_notifications()` | 50 | Notifications | `notifications.rs` |
| `BotsterApp::send_agent_notification()` | 45 | Notifications | `notifications.rs` |
| `BotsterApp::send_heartbeat()` | 80 | Server | `server/heartbeat.rs` |
| `BotsterApp::kill_orphaned_processes()` | 140 | Agents | `agents/process.rs` |
| `BotsterApp::get_parent_pid()` | 30 | Agents | `agents/process.rs` |
| `BotsterApp::spawn_agent_for_message()` | 260 | Agents | `agents/spawn.rs` |
| `BotsterApp::build_web_agent_list()` | 25 | WebRTC | `app/mod.rs` |
| `BotsterApp::close_agent()` | 35 | Agents | `agents/mod.rs` |
| `BotsterApp::handle_cleanup_message()` | 55 | Server | `server/messages.rs` |
| `BotsterApp::handle_webrtc_offer()` | 80 | WebRTC | Keep in main for now |
| `TerminalGuard` | 15 | Core | `app/mod.rs` |
| `run_interactive()` | 620 | Core | `app/mod.rs` |
| `check_for_updates()` | 40 | Update | `update.rs` |
| `update_binary()` | 120 | Update | `update.rs` |
| `run_headless()` | 5 | Core | `app/mod.rs` |
| `main()` | 80 | Entry | `main.rs` |
| `get_prompt()` | 15 | Commands | `commands/prompt.rs` |
| `json_get()` | 25 | Commands | `commands/json.rs` |
| `json_set()` | 50 | Commands | `commands/json.rs` |
| `json_delete()` | 50 | Commands | `commands/json.rs` |
| `delete_worktree()` | 10 | Commands | `commands/worktree.rs` |
| `list_worktrees()` | 80 | Commands | `commands/worktree.rs` |

---

## Conclusion

The Botster Hub CLI has become difficult to maintain due to the god object in `main.rs`. This refactoring plan provides a clear path to:

1. **Immediate wins** (Phases 1-3): Low-risk extractions that improve organization
2. **Structural improvements** (Phases 4-6): Create proper module boundaries
3. **Core refactoring** (Phase 7): Extract the most complex agent management code
4. **Polish** (Phase 8): Documentation and cleanup

Each phase is designed to be:
- Independently deployable
- Easily reversible
- Progressively improving the codebase

The end result will be a maintainable, testable, and AI-friendly codebase that follows Rust best practices.
