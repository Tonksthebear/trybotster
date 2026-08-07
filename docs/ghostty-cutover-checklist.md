# Ghostty upstream snapshot cutover — operator checklist

Cold cutover from the Botster Ghostty fork to **upstream** snapshot format v1.

## Pins

| Item | Value |
|---|---|
| Ghostty | `ghostty-org/ghostty` @ `22d13172cde98a0a4dda05d3d6a3fcb0dd8ed018` |
| Zig | **0.16.0** exact (not 0.15.2) |
| Wire magic | `GHOSTSNP` (+ u16 version 1) |
| Botster opaque label | `ghostty-terminal-snapshot-v1` |

## Repos on main (migration)

- botster-core (PR #118)
- botster-hub (PR #197) — lockstep core packages @ `363602b`
- restty (PR #2)
- trybotster (PR #202) + CI Zig 0.16 (PR #203)

## Operator steps after pull

1. Install Zig 0.16.0 (`mise use zig@0.16.0` or equivalent).
2. For every checkout that vendors Ghostty:
   ```bash
   git submodule sync --recursive
   git submodule update --init --recursive
   git config --get submodule.<path>.url   # must be ghostty-org, not Tonksthebear fork
   ```
3. Rebuild CLI / hub / restty WASM with Zig 0.16 on the build path (`BOTSTER_ZIG` if needed).
4. **Invalidate** stored pre-cutover session snapshots (fork blobs). They will not import.
5. Redeploy/rebuild browser clients that embed restty WASM.

## Security note

GHOSTSNP payloads contain **human-readable terminal cell text** contiguously (not pure opaque noise). Treat logs, diagnostics, and persisted snapshot blobs as **sensitive terminal content** even when base64-wrapped.

## Restorability vs hub green

Hub **does not** decode GHOSTSNP. Green hub CI proves transport/plumbing only. Restorability proof is host encode + restty/client import (and monolith path).

## Known residuals (not blockers for cutover)

| Item | Notes | Follow-up disposition |
|---|---|---|
| CORE-1 callbacks | `semantic_prompt` (OSC 133), `kitty_keyboard_changed` — no upstream equivalent | **Accepted residual.** Full table in `docs/ghostty-upstream-snapshot-v1.md` (CORE-1a/b). No shipping APIs planned. |
| restty kitty_graphics | Upstream `terminal/build_options.zig` disables kitty_graphics on freestanding/WASM | **Accepted residual** of freestanding lib_vt. Not a snapshot-format bug. Track on restty roadmap, not cutover. |
| restty search ABI | Pre-existing reds; design lives in restty `docs/development/search.md` (cooperative WASM step API) | **Out of cutover scope.** Multi-phase product work; do not re-port search as part of GHOSTSNP. |
| trybotster CI scan_js/scan_ruby | npm audit (react-router advisories) / brakeman `--ensure-latest` | **Fixed separately** (`react-router@8.3.0` SPA migration; brakeman 8.0.5). Independent of Ghostty. |
| Release SIGILL risk | Release builds use `-Dcpu=baseline` in trybotster `cli/build.rs` | Re-verify on ship machines. |

## Phase 4 proof (2026-08-07)

- Host encode via libghostty-vt @ pin produced `/tmp/ghostty-phase4/host-export.ghostsnp` (magic `GHOSTSNP`, 1239 bytes).
- Restty main worktree imported it via `RESTTY_EXTERNAL_SNAPSHOT` integration test: **pass**.
- Non-GHOSTSNP blob: import fails closed.
- Release `cargo check --release` with Zig 0.16 + `-Dcpu=baseline` path: **pass** on clean trybotster main.
