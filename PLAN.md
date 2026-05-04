# Plan: Stabilize Workerized Data-Plane Timing And Regression Tests

Ticket: `ticket_1777787368_138401` - Stabilize workerized data-plane timing
and regression tests.

## Intent

This ticket is a carry-forward verification cleanup for the workerized
session-I/O data plane. The production architecture is already in place:
`SessionIoWorker` reads session process frames, coalesces PTY output into
`HubEvent::SessionIoBatch`, preserves ordered structured terminal events, and
lets the hub remain the orchestration owner.

The work is to make that behavior provably deterministic. The existing
coverage should stop relying on sleep-sensitive timing, should explicitly test
the 4 ms time-based flush rule, should prove Bell/Notification/ProcessExit
flush ordering around output, should prove Lua `pty_output` observers see
coalesced batches without byte loss or reordering, and should include a small
deterministic smoke/harness proving the 32 KiB / 4 ms / 16-frame thresholds do
not create visible output latency.

This is not an architecture migration. No old production data path needs to be
replaced. What must be removed or made impossible is the verification gap:
tests that pass by sleeping long enough, tests that only assert one happy burst,
or Lua observer coverage that does not prove total bytes and byte order for
coalesced batches.

## Hard Constraints

- Use `cd cli && ./test.sh ...` for CLI verification, not raw `cargo test`.
- Keep the hub/client-worker/session-I/O ownership split documented in
  `docs/worker-actor-contracts.md`.
- Keep session-process wire protocol unchanged.
- Keep coalescing thresholds unchanged unless a test exposes a real defect:
  32 KiB buffered output, 16 output frames, 4 ms since first buffered output,
  ordered structured event boundaries, EOF/desync/shutdown.
- Do not move hub policy into `worker::session_io` to make tests easier.
- No UI work is planned. `tmp/tailwind_plus_preview` is absent in this
  worktree. If UI work becomes necessary later, use existing React/Vite
  Catalyst components and `IconGlyph`, not new styling primitives.

## Affected Surfaces

- `cli/src/worker/session_io_runtime.rs`
  - Main target for deterministic coalescer/runtime tests.
  - Add or refine test helpers around synthetic encoded frames and hub/event
    receivers.
- `cli/src/hub/server_comms.rs`
  - Add or tighten hub/Lua observer regression coverage for
    `HubEvent::SessionIoBatch` and Lua `pty_output` observer behavior.
- `docs/project-pipelines-verification.md`
  - Record exact focused/full verification commands and results after
    implementation.
- `docs/worker-actor-contracts.md`
  - Only update if implementation discovers a real contract clarification;
    otherwise leave architecture docs unchanged.

## Implementation Sequence

1. Make session-I/O runtime tests deterministic.
   - Replace the sleep-sensitive wait in
     `coalesces_synthetic_output_burst_before_hub_delivery` with a helper that
     drains expected broadcast/hub events until the batch arrives or a bounded
     timeout fails with captured context.
   - Add a reusable test helper for writing encoded frames, shutting down the
     writer when appropriate, and collecting `SessionIoBatch` / exit events
     without arbitrary sleeps.
   - Keep all input synthetic; do not depend on live daemon logs or `/tmp`.

2. Add explicit 4 ms flush coverage.
   - Add a test that writes fewer than 16 frames and less than 32 KiB, keeps
     the stream open, and verifies output flushes because the 4 ms age boundary
     expires.
   - Assert both broadcast `PtyEvent::Output` and hub
     `HubEvent::SessionIoBatch` receive the same bytes in the same order.
   - Avoid using a fragile exact elapsed assertion; the test should prove the
     age-triggered path flushes without requiring frame-count/byte thresholds
     or EOF.

3. Add ordered structured-event boundary tests.
   - Add Bell ordering: output before `FRAME_BELL` flushes before the bell
     notification is observed.
   - Add Notification ordering: output before `FRAME_NOTIFICATION` flushes
     before the OSC notification event.
   - Add ProcessExit ordering: output before `FRAME_PROCESS_EXITED` flushes
     before `HubEvent::SessionProcessExited`, and EOF after process exit still
     emits only one exit.
   - Preserve the intended boundary rule: structured ordered events are not
     coalesced behind later PTY bytes.

4. Add Lua `pty_output` observer byte-order and total-byte assertions.
   - In `cli/src/hub/server_comms.rs`, register a test Lua observer for
     `pty_output` or use the existing Lua test harness if one already exists.
   - Feed multiple `HubEvent::SessionIoBatch` events with coalesced chunks
     that represent several original PTY output frames.
   - Assert Lua observes chunks in order and that concatenated observer bytes
     exactly equal the original expected byte stream.
   - Assert total observed byte count matches the sum of the coalesced batch
     payloads.

5. Add deterministic latency smoke for threshold behavior.
   - Add a focused test/harness that drives the three flush reasons:
     16 frames, 32 KiB, and 4 ms age.
   - Assert each path emits output promptly through the worker/hub event
     channel under a bounded timeout.
   - The point is not a benchmark; it is a regression guard that these
     thresholds cannot silently hold visible output until EOF or a later
     structured event.

6. Update verification docs.
   - Append a dated entry to `docs/project-pipelines-verification.md` with the
     exact commands and results.
   - Include any remaining timing-sensitive test helper rationale if a sleep
     remains unavoidable.

## Verification

Focused CLI checks:

```bash
cd cli && ./test.sh --unit -- session_io_runtime
cd cli && ./test.sh --unit -- server_comms
cd cli && ./test.sh --unit -- worker
```

Full CLI unit verification before handoff:

```bash
cd cli && ./test.sh --unit
```

Static checks:

```bash
rg -n "sleep\\(Duration::from_millis|thread::sleep|tokio::time::sleep" cli/src/worker/session_io_runtime.rs cli/src/hub/server_comms.rs
rg -n "pty_output|SessionIoBatch|FRAME_BELL|FRAME_NOTIFICATION|FRAME_PROCESS_EXITED" cli/src/worker/session_io_runtime.rs cli/src/hub/server_comms.rs
```

Remaining sleeps in production snapshot-redraw code are out of scope. Any
remaining test-only sleep in the touched tests must be justified or replaced.

## Acceptance Checklist

- `coalesces_synthetic_output_burst_before_hub_delivery` no longer depends on
  arbitrary sleep/timing.
- There is explicit coverage for age-based 4 ms flushing independent of EOF,
  16-frame, and 32 KiB triggers.
- Bell, notification, and process-exit ordered boundaries flush pending output
  before the structured event.
- Lua `pty_output` observers have byte-order and total-byte assertions for
  coalesced batches.
- A deterministic smoke/harness proves 32 KiB / 4 ms / 16-frame thresholds do
  not hold visible output indefinitely.
- Verification uses `cli/test.sh` and results are recorded in
  `docs/project-pipelines-verification.md`.
