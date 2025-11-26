# TUI Architecture - Embedded Terminal Implementation

## Overview

This document describes the architecture of the embedded terminal TUI (Terminal User Interface) implementation for the Botster Hub daemon. The system displays multiple agent terminals within a single TUI, allowing users to switch between agents and interact with their shell sessions.

## High-Level Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Botster Hub TUI                         │
│  ┌──────────────┐  ┌────────────────────────────────────┐  │
│  │ Agent List   │  │   Terminal View (70% width)        │  │
│  │ (30% width)  │  │                                    │  │
│  │              │  │  ┌──────────────────────────────┐  │  │
│  │ > repo1#1    │  │  │   VT100 Rendered Terminal    │  │  │
│  │   repo2#2    │  │  │   (tui-term PseudoTerminal)  │  │  │
│  │              │  │  │                              │  │  │
│  │              │  │  │   bash-3.2$ ls               │  │  │
│  │              │  │  │   file1.txt file2.txt        │  │  │
│  │              │  │  │   bash-3.2$ █                │  │  │
│  │              │  │  └──────────────────────────────┘  │  │
│  └──────────────┘  └────────────────────────────────────┘  │
│                                                             │
│  Controls: Ctrl+J/K = switch agents | Ctrl+Q = quit        │
└─────────────────────────────────────────────────────────────┘
```

## Core Components & Libraries

### 1. **ratatui** (v0.29)

- **Purpose**: TUI framework for building terminal user interfaces
- **Usage**: Renders the overall layout, agent list, borders, and layouts
- **Key Features**:
  - Layout system (horizontal/vertical splits with percentage-based sizing)
  - Widget system (List, Block, Paragraph, etc.)
  - Backend abstraction (we use CrosstermBackend)

### 2. **tui-term** (v0.2)

- **Purpose**: Embedded terminal widget for ratatui
- **Usage**: Provides the `PseudoTerminal` widget that renders VT100 terminal state
- **Key Feature**: Takes a `vt100::Screen` and renders it as a ratatui widget
- **Integration**: `PseudoTerminal::new(screen).block(block)` in our render loop

### 3. **vt100** (v0.15.2)

- **Purpose**: VT100/ANSI terminal emulator
- **Usage**: Parses raw PTY output and maintains terminal state (cursor, colors, scrollback)
- **Key API**:
  - `Parser::new(rows, cols, scrollback_lines)` - Initialize with dimensions and scrollback buffer
  - `parser.process(&bytes)` - Feed raw bytes from PTY
  - `parser.screen()` - Get current terminal screen state
  - `parser.set_size(rows, cols)` - Resize terminal
- **Scrollback**: Third parameter (10,000 lines in our case) enables scrollback history

### 4. **portable-pty** (v0.8)

- **Purpose**: Cross-platform PTY (pseudoterminal) library
- **Usage**: Spawns shell processes and provides bidirectional I/O
- **Key API**:
  - `openpty(PtySize)` - Create PTY pair (master + slave)
  - `slave.spawn_command()` - Spawn process in PTY
  - `master.take_writer()` - Get writer for sending input to shell
  - `master.try_clone_reader()` - Get reader for receiving output from shell
  - `master.resize(PtySize)` - Resize PTY (must match VT100 parser size)

### 5. **crossterm** (v0.29)

- **Purpose**: Terminal manipulation library
- **Usage**: Raw mode, event handling, terminal control
- **Key Features**:
  - `enable_raw_mode()` - Capture all keyboard input
  - `event::read()` - Read keyboard/resize events
  - Alternate screen mode
  - Mouse capture

## Data Flow

### Terminal Output (PTY → Screen)

```
┌──────────────┐
│ Shell Process│ (bash/zsh in PTY)
└──────┬───────┘
       │ Raw bytes with ANSI codes
       ↓
┌──────────────┐
│ PTY Master   │
│ Reader Thread│ (background thread reading from PTY)
└──────┬───────┘
       │ Raw bytes (e.g., "\x1b[32mHello\x1b[0m")
       ↓
┌──────────────┐
│ VT100 Parser │ (processes ANSI/VT100 escape sequences)
│ (Arc<Mutex>) │
└──────┬───────┘
       │ Parsed terminal state
       ↓
┌──────────────┐
│ vt100::Screen│ (cursor position, colors, text grid)
└──────┬───────┘
       │ Screen reference
       ↓
┌──────────────┐
│ tui-term     │ (PseudoTerminal widget)
│ Widget       │
└──────┬───────┘
       │ Rendered terminal
       ↓
┌──────────────┐
│ ratatui      │ (draws to terminal)
│ Frame        │
└──────────────┘
```

### User Input (Keyboard → Shell)

```
┌──────────────┐
│ User Keyboard│
└──────┬───────┘
       │ KeyEvent
       ↓
┌──────────────┐
│ crossterm    │ (event::read())
│ Event Loop   │
└──────┬───────┘
       │ Parsed KeyEvent
       ↓
┌──────────────┐
│ Key Handler  │ (key_to_bytes() in main.rs)
│              │ - Ctrl+Q → quit app
│              │ - Ctrl+J/K → switch agents
│              │ - Everything else → convert to bytes
└──────┬───────┘
       │ Raw bytes (e.g., [27, 91, 65] for Up Arrow)
       ↓
┌──────────────┐
│ Agent        │ (write_input(&bytes))
│ Writer       │
└──────┬───────┘
       │ Write bytes to PTY
       ↓
┌──────────────┐
│ PTY Master   │
│ Writer       │
└──────┬───────┘
       │ Bytes to shell
       ↓
┌──────────────┐
│ Shell Process│ (receives input as if from terminal)
└──────────────┘
```

## Critical Implementation Details

### 1. Terminal Sizing

**Problem**: VT100 parser and PTY must have matching dimensions, calculated from the layout.

**Solution** (from tui-term examples):

```rust
// On startup:
let terminal_size = terminal.size()?;
let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);  // 70% width, minus borders
let terminal_rows = terminal_size.height.saturating_sub(2);               // Full height, minus borders

// Initialize agents with these dimensions:
agent.resize(terminal_rows, terminal_cols);

// On Event::Resize:
let terminal_cols = (cols * 70 / 100).saturating_sub(2);
let terminal_rows = rows.saturating_sub(2);
for agent in &agents {
    agent.resize(terminal_rows, terminal_cols);
}
```

**Why this matters**:

- VT100 parser needs correct size to wrap lines properly
- PTY needs correct size so shell (bash/zsh) knows terminal dimensions
- Mismatched sizes cause rendering issues and broken line wrapping

### 2. Resize Synchronization

Both the VT100 parser AND the PTY must be resized together:

```rust
pub fn resize(&self, rows: u16, cols: u16) {
    // Resize the VT100 parser
    {
        let mut parser = self.vt100_parser.lock().unwrap();
        parser.set_size(rows, cols);
    }

    // Resize the PTY to match
    if let Some(master_pty) = &self.master_pty {
        let _ = master_pty.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}
```

**Reference**: See `smux.rs` example from tui-term (lines 278-289)

### 3. PTY Lifecycle Management

**Challenge**: PTY master must be stored for resizing, but reader/writer are consumed.

**Solution** (src/agent.rs:130-207):

```rust
let pair = pty_system.openpty(size)?;

// Clone reader BEFORE taking writer (order matters!)
let mut reader = pair.master.try_clone_reader()?;

// Take writer (consumes writer but not master)
self.writer = Some(pair.master.take_writer()?);

// Use reader in background thread
self.reader_thread = Some(thread::spawn(move || {
    // Read from PTY, feed to VT100 parser
}));

// Store master for future resize operations
self.master_pty = Some(pair.master);
```

### 4. Key-to-Bytes Mapping

Special keys must be converted to proper escape sequences:

```rust
KeyCode::Left => Some(vec![27, 91, 68]),      // ESC [ D
KeyCode::Right => Some(vec![27, 91, 67]),     // ESC [ C
KeyCode::Up => Some(vec![27, 91, 65]),        // ESC [ A
KeyCode::Down => Some(vec![27, 91, 66]),      // ESC [ B
KeyCode::Home => Some(vec![27, 91, 72]),      // ESC [ H
KeyCode::End => Some(vec![27, 91, 70]),       // ESC [ F
KeyCode::PageUp => Some(vec![27, 91, 53, 126]),   // ESC [ 5 ~
KeyCode::PageDown => Some(vec![27, 91, 54, 126]), // ESC [ 6 ~
KeyCode::Delete => Some(vec![27, 91, 51, 126]),   // ESC [ 3 ~
KeyCode::Backspace => Some(vec![8]),          // BS
KeyCode::Enter => Some(vec![b'\n']),          // LF
KeyCode::Tab => Some(vec![9]),                // TAB
```

**Reference**: See `smux.rs` example (lines 310-335) and VT100 escape sequence documentation

## File Structure

```
botster_hub_rs/
├── src/
│   ├── main.rs              # TUI application, event loop, rendering
│   ├── agent.rs             # Agent struct, PTY management, VT100 parser
│   ├── config.rs            # Configuration loading
│   ├── git.rs               # Git worktree management
│   └── lib.rs               # Module exports
├── Cargo.toml               # Dependencies
└── TUI_ARCHITECTURE.md      # This file
```

### Key Files

#### `src/agent.rs` (Agent Management)

- **Agent struct**: Owns PTY, VT100 parser, reader thread
- **Key fields**:
  - `vt100_parser: Arc<Mutex<Parser>>` - Thread-safe terminal emulator
  - `master_pty: Option<Box<dyn MasterPty + Send>>` - For resizing
  - `writer: Option<Box<dyn Write + Send>>` - For sending input
  - `reader_thread: Option<JoinHandle<()>>` - Background thread reading PTY
- **Key methods**:
  - `new()` - Create agent with initial parser (24x80, will be resized)
  - `spawn()` - Create PTY, spawn shell, start reader thread
  - `resize()` - Resize both parser and PTY
  - `write_input()` - Send bytes to shell

#### `src/main.rs` (TUI Application)

- **BotsterApp struct**: Manages agents, selection, rendering
- **Main loop** (lines 270-285):
  1. Render current state (`view()`)
  2. Handle events (`handle_events()`)
  3. Poll server for messages
  4. Sleep 16ms (60 FPS)
- **Event handling**:
  - `Event::Resize` → Recalculate dimensions, resize all agents
  - `KeyCode::Char('q')` + Ctrl → Quit
  - `KeyCode::Char('j')` + Ctrl → Next agent
  - `KeyCode::Char('k')` + Ctrl → Previous agent
  - Everything else → Send to selected agent's PTY
- **Rendering** (lines 162-221):
  - 30/70 horizontal split (agent list / terminal view)
  - PseudoTerminal widget for rendering VT100 screen

## Implemented Features

### Scrollback Navigation ✅

**Scroll Mode** (Ctrl+S to toggle):

- **Normal Mode**: All keys forwarded to PTY (shell interaction)
- **Scroll Mode**: Navigate through 10,000-line scrollback buffer
  - Arrow Up/Down: Scroll by 1 line
  - PageUp/PageDown: Scroll by 20 lines
  - Home: Jump to top of scrollback
  - End: Jump to bottom (live view)
  - Esc: Exit scroll mode
- **Visual Indicators**:
  - Yellow border in scroll mode
  - Scroll position shown in title bar
  - Status bar shows mode (LIVE vs SCROLL)

### Mouse Support ✅

- **Click to select agent**: Click on agent list to switch agents
- **Scroll wheel navigation**:
  - Scroll up: Auto-enter scroll mode, scroll back 3 lines
  - Scroll down: Scroll forward 3 lines, auto-exit when at bottom
- **Status bar help**: Shows "Click=Select Wheel=Scroll"

### Status Bar ✅

Bottom status bar displays:

- Agent status (running/finished/failed)
- Uptime (formatted as seconds/minutes/hours)
- Mode indicator (LIVE or SCROLL with offset)
- Mouse interaction hints

### Keyboard Shortcuts

- **Ctrl+Q**: Quit application
- **Ctrl+S**: Toggle scroll mode
- **Ctrl+J**: Select next agent
- **Ctrl+K**: Select previous agent
- **Esc**: Exit scroll mode (when in scroll mode)
- In scroll mode: Arrow keys, PgUp/PgDn, Home, End for navigation

## Current Limitations & Future Work

### Remaining Limitations

1. **No copy/paste**: Would require implementing a selection mode

### Future Enhancements

1. **Text Selection Mode**:
   - Mouse drag to select text
   - Keyboard selection mode
   - Copy to system clipboard

2. **Search**:
   - Ctrl+F to search terminal output
   - Navigate through search results
   - Highlight matches

3. **Agent Management**:
   - Kill/restart agents from TUI
   - View agent logs separately
   - Filter agent list

## Debugging Tips

### Problem: Terminal doesn't fill height

- Check dimension calculation in `run_interactive()`
- Verify `agent.resize()` is called before `agent.spawn()`
- Check layout constraints in `view()` rendering

### Problem: Input lag or missed keystrokes

- Event poll duration (currently 0ms for instant response)
- Check reader thread is processing PTY output quickly
- Ensure no blocking operations in main loop

### Problem: Terminal rendering is garbled

- VT100 parser size mismatch with PTY
- Check both are resized together in `Agent::resize()`
- Verify escape sequence handling in `key_to_bytes()`

### Problem: "Failed to initialize input reader"

- Usually from running in background without proper TTY
- Run directly in terminal: `cargo run -- start`
- Not from `./target/release/botster-hub` (rebuild with `cargo build --release`)

## References

- [tui-term GitHub examples](https://github.com/a-kenji/tui-term/tree/development/examples)
  - `smux.rs` - Terminal multiplexer (most relevant to our use case)
  - `simple_ls_rw.rs` - Basic PTY output rendering
- [ratatui documentation](https://docs.rs/ratatui/0.29.0/ratatui/)
- [vt100 crate docs](https://docs.rs/vt100/0.15.2/vt100/)
- [portable-pty docs](https://docs.rs/portable-pty/0.8.0/portable_pty/)
- [VT100 escape sequences reference](https://vt100.net/docs/vt100-ug/chapter3.html)

## Build & Run

```bash
# Development build and run
cd botster_hub_rs
cargo run -- start

# Release build
cargo build --release
./target/release/botster-hub start

# With debug logging
RUST_LOG=debug cargo run -- start
```

## Summary

This TUI implementation uses a proven architecture from the tui-term examples:

- **ratatui** for layout and widgets
- **tui-term** for embedded terminal rendering
- **vt100** for terminal emulation
- **portable-pty** for shell process management

The critical insight is that the VT100 parser and PTY must have synchronized dimensions that match the actual widget area in the layout. All keyboard input is converted to proper escape sequences and forwarded to the shell, while PTY output is continuously parsed by the VT100 emulator and rendered by the tui-term widget.
