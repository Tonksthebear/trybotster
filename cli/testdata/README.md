# VT SIGSEGV regression fixtures

Offline repros for hard session deaths (`signal 11` / exit `139`) inside
`libghostty-vt` while the session reader calls `process()` / `vt_write`.

**How to run (always via test script or `BOTSTER_ENV=test`):**

```bash
cd cli
cargo build   # ensures target/debug/botster exists for lib subprocess tests
./test.sh --lib botster_vt_
# or:
BOTSTER_ENV=test cargo test --lib botster_vt_ -- --nocapture
```

**Compare trybotster pin vs pure upstream:**

```bash
cd cli
./scripts/vt-segv-matrix.sh both
# pin only:
./scripts/vt-segv-matrix.sh pin
# ghostty-org/main tip only (will detach vendor/ghostty; restores after):
./scripts/vt-segv-matrix.sh upstream
```

See script help: `./scripts/vt-segv-matrix.sh --help`.

---

## Failure classes we learned (keep explicit)

| ID | Name | Symptom | Stack / mechanism | Pin defense (trybotster/ghostty) |
|----|------|---------|-------------------|----------------------------------|
| **A** | OSC-8 hyperlink start | Long agent TUI, many unique OSC-8 | `startHyperlink` → `increaseCapacity` → stack `EXC_BAD_ACCESS` | Silent degrade on page pressure; no log |
| **B** | Unique truecolor styles | Dense unique `CSI 38;2;…;48;2;…m` | `manualStyleUpdate` → `increaseCapacity(.styles)` | Single `styles.add`; degrade to default style_id |
| **C** | Hyperlink on cell paint | Active OSC-8 + print | `print` → `cursorSetHyperlink` → `increaseCapacity(.hyperlink_bytes)` | On `HyperlinkMapOutOfMemory` leave cell unlinked |
| **D** | VT `log.warn` during parse | Invalid multi-param CUP / unimplemented CSI mid-stream | `csiDispatch` → `log.warn` → lib-vt `logFn`/`emitLog` stack blow | `logFn` no-ops unless `.err`; host callback drops non-errors |

Related upstream (different manifestation, same family): [ghostty-org/ghostty#11261](https://github.com/ghostty-org/ghostty/issues/11261) capacity changes during print / stale pointers. Our pins are **session-survival degrades**, not a full upstream root fix of page clone.

---

## Fixtures

Each dir is a `botster debug vt-replay` input: `x.vtring` (ring) and/or `x.vtlast` (last chunk). Replay size **70×226** unless noted.

| Directory | Class | Origin | Notes |
|-----------|-------|--------|--------|
| `vt_crash_min/` | **A** | Original Botster capture | First offline OSC-8-heavy min repro |
| `vt_style_pressure/` | **B** | Synthetic | ≥200 unique truecolor SGR cells; unfixed → exit 139 at ~200 |
| `vt_crash_hyperlink_map/` | **C**+**D** | Production `sess-1786319046-0003-…` | Full dump; offline RED on unfixed pin; GREEN on `5e9ba17a2+` |
| `vt_invalid_cup/` | **D** (minimal) | Same session ring + last = `56;6H` only | Completes mid-CSI to invalid 3-param CUP; isolates emitLog path |

Layout for a fixture:

```text
testdata/<name>/
  x.vtring   # optional but usual
  x.vtlast   # optional; last write at death
```

Manual one-shot:

```bash
./target/debug/botster debug vt-replay --quiet --rows 70 --cols 226 testdata/vt_invalid_cup
echo exit:$?
```

---

## Ghostty pin (Botster)

Tracked in submodule `cli/vendor/ghostty` → `https://github.com/trybotster/ghostty.git` branch `botster-vt-segv-fix`.

Keep monorepo, botster-core, restty, and hub lock on the **same** tip when the pin moves (GHOSTSNP + session survival).

Zig unit tests on the fork (optional, not monorepo CI):

- `Screen: Botster OSC8 capacity pressure does not crash`
- `Screen: Botster unique truecolor style pressure does not crash`

---

## Adding a new class

1. Capture dump (`*.vtring` / `*.vtlast` / hub `signal=Some(11)` log).
2. Prove **RED** offline on current pin (`exit 139` / signal 11).
3. Isolate minimal stream; name the class in the table above.
4. Add `testdata/<name>/` + one `botster_vt_*_does_not_sigsegv` test (use `assert_vt_replay_fixture_no_sigsegv`).
5. Fix pin; prove **GREEN**; run `./scripts/vt-segv-matrix.sh both` before merge.
