# Ghostty upstream snapshot cutover (trybotster Phase 2b)

## Pin

| Constant | Value |
|----------|--------|
| Ghostty | ghostty-org/ghostty @ `22d13172cde98a0a4dda05d3d6a3fcb0dd8ed018` |
| Zig | **exactly** `0.16.0` (mise + build preflight) |
| Snapshot label | `ghostty-terminal-snapshot-v1` |
| Wire magic | `GHOSTSNP` (u16 format version follows magic) |

The Tonksthebear Ghostty fork is retired. Submodule: `cli/vendor/ghostty`.

### Existing checkouts — submodule URL sync

`.gitmodules` now points at ghostty-org, but **already-cloned worktrees keep the
old Tonksthebear URL in `.git/config`** until synced:

```bash
git submodule sync --recursive
git submodule update --init cli/vendor/ghostty
cd cli/vendor/ghostty && git rev-parse HEAD   # expect 22d13172…
```

## Snapshot API

- **Export:** `ghostty_snapshot_encode_alloc` → typed `Result` (`SnapshotError`), not silent `Option`
- **Import:** `decoder_new_buf` → `decode` → `decoder_free`
- **Decode produces a caller-owned terminal** (handle swap after success only)
- **Continuation:** `GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES` (31) = 1024 at create; **re-armed after every import**
- **Cold cutover:** old fork-format blobs fail closed; no dual-decode

## Callbacks

Kept (upstream OPT exists):

- write_pty, bell, title_changed, pwd_changed, color_scheme
- **desktop_notification** (renamed from fork `Notification`; sized struct)

Replaced:

- **mode_changed** → poll via `ghostty_terminal_get` + `GHOSTTY_TERMINAL_DATA_MODE` / mode flags after each VT write

## Hard regressions (documented only)

Upstream no longer exposes first-class hooks for:

1. **OSC 133 semantic prompt marks** — session no longer emits `FRAME_PROMPT_MARK` from a Ghostty callback. No byte-scanning reimplementation.
2. **Kitty keyboard change notifications** — no dedicated push callback. Kitty enablement still appears in polled mode flags (`kitty_enabled`) when it changes with other mode polls, but there is no OSC-style push event path.

## Build

```text
zig build -Demit-lib-vt -Doptimize=ReleaseFast -Dsimd=false -Dcpu=baseline \
  -Dversion-string=1.3.2-dev -Demit-xcframework=false
```

`-Demit-xcframework=false` is required for lib-vt on macOS Command Line Tools.

## Geometry after import (acceptance bar #14)

Upstream decode sizes the **new** terminal to the snapshot dimensions. TUI
`TerminalPanel::on_scrollback_with_dims` must re-read `term.rows()`/`cols()`
into `self.dims` after a successful import. Leaving caller dims cached makes
`resize()` early-return and swallow the corrective layout size.

## Callbacks after import (punch-list blocker)

Decode replaces the terminal handle. Host userdata and OPT callbacks are
**not** part of the snapshot. Import must go through
`TerminalParser::snapshot_import`, which re-installs userdata, write_pty,
title/bell/pwd/desktop_notification, the builtin color-scheme hook, and
re-seeds `color_cache` onto the new handle.

## Gates

- workspace `clippy -D warnings` (cli)
- `git diff --check`
- `cd cli && ./test.sh` (not bare `cargo test`)
