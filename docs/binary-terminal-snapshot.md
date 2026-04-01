# Binary Terminal State Transfer

## Goal

Two ghostty terminal instances (potentially on different machines, different targets — arm64 native and wasm32) need to be perfectly synchronized. One is the **source of truth** (session process), the other is a **viewer** (TUI client or browser via restty WASM).

## The Operation

```
Source terminal → export() → opaque binary blob → import() → Destination terminal
```

After import, the destination terminal must be **observationally identical** to the source. Same cells, same scrollback, same cursor position, same colors, same modes, same everything.

## What "Identical" Means

- All cell content (characters, graphemes, wide chars)
- All cell attributes (SGR styles, colors, hyperlinks, semantic content, protection)
- All row metadata (wrap flags, semantic prompt marks)
- Full scrollback history
- Cursor position, style, and pending_wrap state
- Saved cursor (DECSC/DECRC)
- Both screens — primary (with scrollback) and alternate (if initialized)
- Which screen is active
- Terminal modes (all DEC/ANSI modes + saved + defaults)
- Color palette (current 256 entries + overrides + defaults)
- Scrolling region
- Tab stops
- Charset state (G0-G3, GL, GR, single shift)
- Kitty keyboard protocol stack
- PWD and title

## API

Two functions — that's it:

```zig
// Source side: serialize entire terminal as one opaque blob
ghostty_snapshot_terminal_export(terminal, alloc, out_ptr, out_len) -> Result

// Destination side: clear terminal, load blob, become identical
ghostty_snapshot_terminal_import(terminal, data, len) -> Result
```

## Wire Format

```
[u8: TERMINAL_SNAPSHOT_VERSION]
[u16 LE: cols]
[u16 LE: rows]
[u8: screen_count]
[u8: active_screen_key (0=primary, 1=alternate)]

Per screen:
  [u8: screen_key]
  [u32 LE: blob_len]
  Screen blob:
    [u8: SCREEN_VERSION]
    [u32 LE: page_count]
    Per page:
      [u32 LE: memory_len]
      [u16 LE: used_cols][u16 LE: used_rows]
      [u16 LE: cap_cols][u16 LE: cap_rows][u16 LE: cap_styles]
      [u32 LE: cap_grapheme_bytes][u16 LE: cap_hyperlink_bytes][u32 LE: cap_string_bytes]
      [memory_len bytes: raw page backing memory]
    Cursor state: x, y, style, pending_wrap, protected, SGR style, saved cursor
    Charset state: GL, GR, single_shift, G0-G3
    Kitty keyboard: idx + 8 flags

Terminal state:
  scrolling_region (4 x u16)
  modes (3 x ModePacked backing int)
  colors (dynamic RGB overrides + 256 palette + mask)
  tabstops (cols + prealloc_stops + dynamic_stops)
  pwd (length-prefixed string)
  title (length-prefixed string)
  flags (u128 wide)
```

## Cross-Platform Requirement

Pages must be layout-identical between arm64 (native CLI) and wasm32 (restty browser). `Offset.Slice.len` was changed from `usize` to `u32` (OffsetInt) to fix the main blocker. All other page memory types (Cell, Row, Offset, StyleSet entries) use fixed-size packed structs.

## Known Bugs (Fixed — Need Tests to Prove)

1. **Fresh parser page append** — destination terminal starts with a non-empty initial page. page_load was appending instead of replacing. **Fix:** clearPageList before loading, update PageList.cols/rows from blob header before reset.

2. **Alt screen not initialized** — ghostty lazy-inits the alternate screen. Import must call `getInit(.alternate)` to create it before loading alt pages. **Fix:** Force-init via getInit if screen not present.

3. **Stale alt screen** — if destination has an alt screen but blob doesn't, stale state survives. **Fix:** Remove alt screen after import if not in blob.

4. **Dimension mismatch** — PageList.reset() uses its own cols/rows, not the blob's. **Fix:** Set PageList.cols/rows from blob header BEFORE reset.

5. **Unsafe casts in state_import** — `@intCast`/`@enumFromInt` on values from the blob can hit unreachable on invalid data. **Fix:** Use `math.cast`/`intToEnum` with fallback defaults.

6. **Empty pwd/title not clearing existing values** — clearRetainingCapacity was inside `if len > 0`. **Fix:** Always clear, then conditionally fill.

## What Tests Need to Prove

1. **Basic round-trip**: export terminal with content -> import into fresh terminal -> cells match
2. **Scrollback round-trip**: terminal with 1000+ lines of scrollback -> export -> import -> scrollback intact, same content
3. **Cursor position**: write content that positions cursor at specific location -> export -> import -> cursor at same position
4. **Alt screen active**: enter alt screen, write content -> export -> import -> alt screen content correct, primary screen + scrollback preserved
5. **Alt screen inactive but initialized**: enter alt screen, exit, write more -> export -> import -> primary is active, alt screen data preserved
6. **Modes round-trip**: set various modes (bracketed paste, mouse, kitty keyboard, etc.) -> export -> import -> all modes match
7. **Colors round-trip**: set custom palette entries + fg/bg overrides -> export -> import -> colors match
8. **Styles round-trip**: write content with bold, italic, colored text -> export -> import -> SGR styles intact
9. **Wide chars + graphemes**: write emoji, CJK, grapheme clusters -> export -> import -> all intact
10. **Hyperlinks**: write OSC 8 hyperlinks -> export -> import -> hyperlinks preserved
11. **Empty terminal**: export fresh terminal -> import -> no crash, identical empty state
12. **Dimension mismatch**: export 80x24 terminal -> import into 120x40 terminal -> terminal resized to 80x24, content correct

## How This Gets Used

```
Session process:
  terminal_export(session_terminal) -> blob
  Send blob over Unix socket as FRAME_SNAPSHOT

Hub:
  Routes blob unchanged (opaque)

TUI client (Rust, same platform):
  terminal_import(tui_terminal, blob) -> identical terminal

Browser client (restty WASM, different platform):
  restty.loadBinarySnapshot(blob) -> internally calls terminal_import -> identical terminal
```

After initial sync, both clients receive raw PTY bytes via broadcast and process them through their own ghostty parser — staying in sync from that point forward.

## Prior Art

### zmx (github.com/neurosnap/zmx)
Uses ghostty's `TerminalFormatter` with VT format for state transfer on re-attach. Has open bugs with cursor corruption (#98), prompt not repainting (#99), and scrollback leaking (#106). These are fundamental VT replay issues.

### DAmesberger/ghostty SSH remote sessions
Tried binary page diffs, found "too many edge cases" (cursor sync, screen clearing, images, graphemes), reverted to VT for viewport + binary cell streaming for scrollback only. Their binary scrollback format serializes individual cells as u64 packed values with grapheme data and style ID remapping — platform-independent by design.

### Why Binary Page Transfer Is Better (When It Works)
- Lossless — captures everything VT encoding drops (hyperlinks, semantic prompts, saved cursor, per-cell protection)
- No VT round-trip bugs (cursor drift, scrollback interference, mode sequencing)
- Ghostty's page architecture is designed for memcpy (offset-based addressing)
- One format for all clients (TUI and browser both run ghostty)
