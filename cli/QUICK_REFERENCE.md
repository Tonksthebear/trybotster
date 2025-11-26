# Botster Hub TUI - Quick Reference

## Starting the TUI

```bash
cd botster_hub_rs
cargo run -- start

# Or with release build
cargo build --release
./target/release/botster-hub start
```

## Keyboard Shortcuts

### Global Controls
| Key | Action |
|-----|--------|
| `Ctrl+Q` | Quit application |
| `Ctrl+J` | Select next agent |
| `Ctrl+K` | Select previous agent |
| `Ctrl+S` | Toggle scroll mode |

### Normal Mode (Live Terminal)
- All keys are forwarded to the shell
- Type commands and interact with the terminal normally
- Green "LIVE" indicator in status bar

### Scroll Mode (Scrollback Navigation)
| Key | Action |
|-----|--------|
| `↑` / `↓` | Scroll up/down by 1 line |
| `PgUp` / `PgDn` | Scroll up/down by 20 lines |
| `Home` | Jump to top of scrollback |
| `End` | Jump to bottom (live view) |
| `Esc` | Exit scroll mode |

**Visual indicators in scroll mode:**
- Yellow border around terminal
- Title shows scroll position (e.g., "↑150/5000 lines")
- Yellow "SCROLL" indicator in status bar

## Mouse Controls

| Action | Effect |
|--------|--------|
| Click on agent | Switch to that agent |
| Scroll wheel up | Enter scroll mode, scroll back 3 lines |
| Scroll wheel down | Scroll forward 3 lines |

**Smart behavior:**
- Scrolling down to the bottom automatically exits scroll mode
- Switching agents resets scroll position to bottom

## Status Bar

Bottom status bar shows:
- **Status**: Agent state (running/finished/failed)
- **Uptime**: How long the agent has been running
- **Mode**: LIVE (green) or SCROLL (yellow) with offset
- **Mouse hints**: Reminder of mouse controls

## Layout

```
┌──────────────────────────────────────────────────────────┐
│ Agents (2) [Ctrl+J/K]  │ repo1#1 [Ctrl+Q quit | Ctrl+S  │
│ ├─────────────────────┐│                                 │
│ │ > repo1#1           ││  $ ls                           │
│ │   repo2#2           ││  file1.txt  file2.txt           │
│ └─────────────────────┘│  $ █                            │
│                        │                                 │
└──────────────────────────────────────────────────────────┘
Status: running | Uptime: 5m | Mode: LIVE | Mouse: Click=Select
```

## Tips

1. **Reviewing long output**: Use `Ctrl+S` to enter scroll mode, then `Home` to jump to the beginning
2. **Quick navigation**: Use mouse wheel for fast scrolling through recent output
3. **Switching agents**: Either click the agent name or use `Ctrl+J`/`Ctrl+K`
4. **Getting unstuck**: Press `Esc` to exit scroll mode if keys aren't working

## Scrollback Buffer

- Each agent maintains a 10,000-line scrollback buffer
- Accessed via `get_vt100_with_scrollback()` method
- Automatically captures all terminal output including ANSI colors and formatting
- VT100 parser handles escape sequences properly

## Architecture

- **Normal Mode**: Uses `tui-term::PseudoTerminal` widget for live VT100 rendering
- **Scroll Mode**: Renders scrollback history as `ratatui::Paragraph` widget
- **Mouse events**: Captured via `crossterm::EnableMouseCapture`
- **Resize handling**: Automatically resizes both VT100 parser and PTY together

## Troubleshooting

### Keys not responding
- Check if you're in scroll mode (yellow border)
- Press `Esc` to exit scroll mode

### Terminal output looks garbled
- VT100 parser and PTY sizes should match
- Try resizing the terminal window (triggers recalculation)

### Can't see old output
- Press `Ctrl+S` to enter scroll mode
- Use arrow keys or `PgUp` to scroll back
- Current limit: 10,000 lines

### Mouse not working
- Ensure terminal emulator supports mouse events
- Most modern terminals (iTerm2, Alacritty, etc.) support this
- Try keyboard shortcuts as fallback
