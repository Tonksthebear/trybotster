# Workerized Web/TUI Connection Repair Evidence

Ticket: `ticket_1777870115_155594`

Date: 2026-05-04

## Intent Coverage

- Browser, TUI, and socket terminal attach paths keep the workerized architecture: Hub owns attach orchestration, `ClientWorker` owns subscription/input stream state, and Session I/O remains the session data plane.
- Missing session-I/O sender registration is now observable to operators and users. Subscribed input with no sender emits a `terminal_attach` control frame with state `not_ready`, logs the drop, increments hub metrics through `client_worker.session_io_missing`, opens the TUI's existing error mode, and emits the browser terminal connection's existing `error` event with reason `terminal_not_ready`.
- The fixed first-attach snapshot sleeps and the resize bounce were removed cold turkey from the WebRTC initial attach, browser refresh snapshot, and shared TUI/socket terminal runtime paths. Regression coverage prevents reintroducing `thread::sleep` or the former `125ms` delay in those functions.

## Files Changed

- `cli/src/worker/client.rs`
  - Added `TerminalAttachState::NotReady` with wire value `not_ready`.
  - Changed subscribed-input-without-sender from a silent return into a `not_ready` control frame plus observable hub-control backpressure event.
  - Added regression coverage for missing sender and closed stale sender behavior.
- `cli/src/hub/server_comms.rs`
  - Removed fixed `25ms`/`125ms` attach-path sleeps and the `should_force_snapshot_redraw` resize bounce from WebRTC initial attach, browser refresh snapshot, and shared TUI/socket snapshot paths.
  - Added metrics for client-worker backpressure and missing session-I/O sender events.
  - Added regression coverage for fixed-sleep removal, observable missing-sender metrics, and coalesced socket-frame reading in reconnecting attach tests.
- `cli/src/clients/tui/runner.rs`
  - Handles `terminal_attach` state `not_ready` by rendering the existing error modal with a session-specific message and syncing Lua mode to `error`.
  - Added regression coverage that the TUI renders `not_ready` as user-visible error state.
- `app/frontend/lib/connections/terminal_connection.js`
  - Handles `terminal_attach` frames, emits `terminalAttach`, and surfaces `not_ready` via the existing `error` event as `terminal_not_ready`.
- `app/frontend/lib/transport/hub_peer_connection.js`
  - Carries WebRTC offer-to-data-channel-open timing on the `connection:state` event.
  - Waits for the hub-side DataChannel ready marker before measuring subscribe-ready timing, then emits `subscription:ready` after the worker-routed subscribed ack arrives.
- `app/frontend/lib/connections/hub_route.js`
  - Emits page-visible browser timing logs for peer-ready and subscribe-ready spans consumed by system tests.
- `app/frontend/test/terminal-connection.test.js`
  - Covers browser-side `terminal_attach` `not_ready` error emission.
- `docs/diagnostics/workerized-web-tui-connection-repair.md`
  - Captures implementation evidence, commands, and smoke-test results for the pipeline gate.

## Verification

Commands run:

```bash
cd cli
./test.sh --unit -- worker::client::tests::subscribed_input_without_session_io_sender_emits_not_ready
./test.sh --unit -- hub::server_comms::tests::test_terminal_attach_snapshot_paths_have_no_fixed_sleep_settle_windows
./test.sh --unit -- test_tui_first_scrollback_latency_budget_session_backed -- --nocapture
./test.sh --unit -- terminal_attach
./test.sh --unit -- attach_reconnecting
./test.sh --unit -- missing_session_io_sender
./test.sh --unit -- closed_session_io_sender_is_removed_then_next_input_is_not_ready
./test.sh --unit -- test_socket_attach_reconnecting_emits_explicit_attach_state
./test.sh --unit -- terminal_attach_not_ready_renders_error_mode
./test.sh --unit
cargo build --bin botster
cd ..
npx vitest run app/frontend/test/terminal-connection.test.js app/frontend/test/hub-peer-connection-peer-lost.test.js
bin/rails db:create
bin/rails db:migrate
bin/rails test test/system/webrtc_connection_test.rb -i 'browser establishes WebRTC connection with CLI'
bin/rails test test/system/webrtc_connection_test.rb -i 'browser reconnects after hub reboot with preserved keys'
cd cli
BOTSTER_ENV=test ./target/debug/botster start --headless
BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c
BOTSTER_ENV=test ./target/debug/botster start --headless --offline
BOTSTER_ENV=test ./target/debug/botster status
BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c
BOTSTER_ENV=test ./target/debug/botster attach
```

Results:

- Focused missing-sender, fixed-sleep, and TUI `not_ready` render tests passed.
- Focused TUI first scrollback timing budget test passed: p95=0.13ms, p99=0.33ms over 40 session-backed samples with 500 lines of scrollback.
- Focused `terminal_attach` tests passed: 4 passed.
- Focused `attach_reconnecting` tests passed: 2 passed.
- Focused frontend tests passed: 2 files, 12 tests.
- Full CLI unit suite passed after the returned-finding fixes: 1532 passed, 0 failed, 1 ignored, finished in 185.16s.
- `cargo build --bin botster` passed. It emitted one existing warning about unused WebRTC registry methods.
- Browser/WebRTC production-path evidence:
  - Test databases were missing in the worktree, so `bin/rails db:create` was run and created the development/test, queue, and cable databases.
  - `bin/rails db:migrate` then completed successfully.
  - `config/credentials/test.key` was copied from the canonical repo into this worktree per human instruction and is ignored by git.
  - The targeted system test `bin/rails test test/system/webrtc_connection_test.rb -i 'browser establishes WebRTC connection with CLI'` passed after the returned finding fix: 1 run, 11 assertions, 0 failures, 0 errors, 0 skips. It recorded peer-ready 785ms and `Browser WebRTC first-connect subscribe-ready timing p95=394ms p99=394ms samples=[394]`.
  - The targeted reconnect system test `bin/rails test test/system/webrtc_connection_test.rb -i 'browser reconnects after hub reboot with preserved keys'` passed after the returned finding fix: 1 run, 13 assertions, 0 failures, 0 errors, 0 skips. It recorded peer-ready 787ms and `Browser WebRTC reconnect subscribe-ready timing p95=396ms p99=396ms samples=[396]`.
- Non-offline headless hub smoke:
  - First sandboxed `start --headless` failed because the sandbox could not write `~/.botster-dev/.../hub.lock`.
  - Escalated `BOTSTER_ENV=test ./target/debug/botster start --headless` succeeded and logged `Hub ready. Waiting for connections...`.
  - `BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c` returned a `https://dev.trybotster.com/hubs/device-14ee1782b811190c/pairing#...` URL, proving non-offline relay URL generation in this worktree.
  - The smoke hub was stopped before the Rails system-test retry.
- Real local hub smoke:
  - First sandboxed `start --headless --offline` failed because the sandbox could not write `~/.botster-dev/.../hub.lock`.
  - Escalated smoke start succeeded and logged `Hub ready. Waiting for connections...`.
  - `botster status` reported the hub process alive and socket protocol healthy: `path_exists=true, connectable=true, protocol=true`, diagnosis `hub accepts new local IPC clients`.
  - `get-connection-url --hub device-14ee1782b811190c` returned `No connection URL found...` because offline mode does not generate a browser relay URL. Browser timing evidence comes from the headless Chrome system tests above.
  - `botster attach` connected to the running device hub and rendered the TUI frame with sessions and terminal panes. That smoke hub had no sessions, so live-session-backed TUI timing evidence comes from the focused session-backed harness above.
  - The attach process and headless hub were stopped after the smoke.

## Timing Evidence

Automated regression coverage verifies the latency-affecting code changes directly and now enforces the plan budgets in the focused browser/TUI paths:

- The three former fixed attach delays and resize-bounce calls are gone from the first-attach snapshot functions.
- `snapshot.rpc_get` and `snapshot.gzip_queue` instrumentation remains in the snapshot paths.
- Missing session-I/O registration is surfaced through `not_ready` control egress, `client_worker.session_io_missing` metric, TUI error mode, and browser terminal error event.

Measured p95/p99 budget table:

| Path | Budget | Observed | Evidence |
| --- | --- | --- | --- |
| TUI first scrollback | p95 < 250ms, p99 < 500ms | p95=0.13ms, p99=0.33ms, 40 samples | `./test.sh --unit -- test_tui_first_scrollback_latency_budget_session_backed -- --nocapture` |
| Browser first connect | p95 < 750ms, p99 < 1500ms | p95=394ms, p99=394ms, 1 real headless-browser sample | `bin/rails test test/system/webrtc_connection_test.rb -i 'browser establishes WebRTC connection with CLI'` |
| Browser reconnect | p95 < 1000ms, p99 < 2000ms | p95=396ms, p99=396ms, 1 real headless-browser sample | `bin/rails test test/system/webrtc_connection_test.rb -i 'browser reconnects after hub reboot with preserved keys'` |

Browser peer-ready timings are measured from encrypted WebRTC offer send to browser DataChannel open. The pass/fail browser budget is measured after the hub has also processed DataChannel open and sent the `dc_ready` transport marker: hub-side DataChannel ready to encrypted subscribe send to worker-routed subscribed ack. This keeps terminal subscribe ownership in `ClientWorker` while excluding the WebRTC handshake and hub open-event scheduling from the post-handshake subscribe budget.

The returned verifier finding exposed an important measurement bug. Earlier subscribe-ready samples around 1083-1231ms started at `HubRoute` before the browser DataChannel was open; the logs showed `subscribe start` before `peer ready`, so the metric included WebRTC/hub readiness wait. The corrected implementation starts the budget timer inside `HubPeerConnection.subscribe()` only after browser DataChannel open and hub `dc_ready`. The system test now always enforces p95, even with one sample.

The TUI timing harness uses the production session-backed attach branch with a test-only snapshot override so it exercises the live-session path without requiring an external PTY process.

Observed command durations:

- Full unit suite: 185.16s on the final diff.
- Focused reconnect attach tests: about 22s in the test harness.
- Local hub status smoke confirmed connectable IPC immediately after hub readiness.
- Non-offline headless hub reached readiness and generated a browser pairing URL.
