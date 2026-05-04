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
- `cli/src/tui/runner.rs`
  - Handles `terminal_attach` state `not_ready` by rendering the existing error modal with a session-specific message and syncing Lua mode to `error`.
  - Added regression coverage that the TUI renders `not_ready` as user-visible error state.
- `app/frontend/lib/connections/terminal_connection.js`
  - Handles `terminal_attach` frames, emits `terminalAttach`, and surfaces `not_ready` via the existing `error` event as `terminal_not_ready`.
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
./test.sh --unit -- terminal_attach
./test.sh --unit -- attach_reconnecting
./test.sh --unit -- missing_session_io_sender
./test.sh --unit -- closed_session_io_sender_is_removed_then_next_input_is_not_ready
./test.sh --unit -- test_socket_attach_reconnecting_emits_explicit_attach_state
./test.sh --unit -- terminal_attach_not_ready_renders_error_mode
./test.sh --unit
cargo build --bin botster
cd ..
npx vitest run app/frontend/test/terminal-connection.test.js
bin/rails db:create
bin/rails db:migrate
bin/rails test test/system/webrtc_connection_test.rb -i 'browser establishes WebRTC connection with CLI'
cd cli
BOTSTER_ENV=test ./target/debug/botster start --headless
BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c
BOTSTER_ENV=test ./target/debug/botster start --headless --offline
BOTSTER_ENV=test ./target/debug/botster status
BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c
BOTSTER_ENV=test ./target/debug/botster attach --hub device-14ee1782b811190c
```

Results:

- Focused missing-sender, fixed-sleep, and TUI `not_ready` render tests passed.
- Focused `terminal_attach` tests passed: 4 passed.
- Focused `attach_reconnecting` tests passed: 2 passed.
- Focused frontend `TerminalConnection` test passed: 1 passed.
- Full CLI unit suite passed after the returned-finding fixes: 1531 passed, 0 failed, 1 ignored, finished in 185.53s.
- `cargo build --bin botster` passed. It emitted one existing warning about unused WebRTC registry methods.
- Browser/WebRTC production-path attempt:
  - Test databases were missing in the worktree, so `bin/rails db:create` was run and created the development/test, queue, and cable databases.
  - `bin/rails db:migrate` then completed successfully.
  - The targeted system test `bin/rails test test/system/webrtc_connection_test.rb -i 'browser establishes WebRTC connection with CLI'` reached Puma/Capybara but failed before pairing because the worktree lacks Rails test credentials: `ActiveRecord::Encryption::Errors::Configuration: Missing Active Record encryption credential: active_record_encryption.deterministic_key`.
  - This means the existing headless-browser WebRTC test is present and was attempted, but the current worktree cannot execute it without the Rails test encryption credential.
- Non-offline headless hub smoke:
  - First sandboxed `start --headless` failed because the sandbox could not write `~/.botster-dev/.../hub.lock`.
  - Escalated `BOTSTER_ENV=test ./target/debug/botster start --headless` succeeded and logged `Hub ready. Waiting for connections...`.
  - `BOTSTER_ENV=test ./target/debug/botster get-connection-url --hub device-14ee1782b811190c` returned a `https://dev.trybotster.com/hubs/device-14ee1782b811190c/pairing#...` URL, proving non-offline relay URL generation in this worktree.
  - The smoke hub was stopped before the Rails system-test retry.
- Real local hub smoke:
  - First sandboxed `start --headless --offline` failed because the sandbox could not write `~/.botster-dev/.../hub.lock`.
  - Escalated smoke start succeeded and logged `Hub ready. Waiting for connections...`.
  - `botster status` reported the hub process alive and socket protocol healthy: `path_exists=true, connectable=true, protocol=true`, diagnosis `hub accepts new local IPC clients`.
  - `get-connection-url --hub device-14ee1782b811190c` returned `No connection URL found...` because offline mode does not generate a browser relay URL. Browser relay timing could not be measured in this offline smoke.
  - `attach --hub device-14ee1782b811190c` connected to the running hub and rendered the TUI frame with sessions and terminal panes. There were no sessions in the smoke hub, so terminal scrollback latency could not be measured against a live session.
  - The attach process and headless hub were stopped after the smoke.

## Timing Evidence

Automated regression coverage verifies the latency-affecting code changes directly:

- The three former fixed attach delays and resize-bounce calls are gone from the first-attach snapshot functions.
- `snapshot.rpc_get` and `snapshot.gzip_queue` instrumentation remains in the snapshot paths.
- Missing session-I/O registration is surfaced through `not_ready` control egress, `client_worker.session_io_missing` metric, TUI error mode, and browser terminal error event.

Observed command durations:

- Full unit suite: 185.53s on the final diff.
- Focused reconnect attach tests: about 22s in the test harness.
- Local hub status smoke confirmed connectable IPC immediately after hub readiness.
- Non-offline headless hub reached readiness and generated a browser pairing URL, but no browser p95/p99 could be captured because the Rails system test is blocked on missing `active_record_encryption.deterministic_key`.

The requested p95/p99 browser and live-session TUI budgets require a real browser relay with a runnable browser system-test environment and a live session with scrollback. This worktree validates hub IPC/TUI attach, non-offline pairing URL generation, and code-level latency removals, but cannot capture browser/WebRTC p95/p99 or live-session TUI p95/p99 without the missing Rails test encryption credential and a live scrollback session. Human waiver questions `question_1777917988_639601` and `question_1777921129_432878` are pending for the browser relay/live-session measurement requirement; this diagnostic should not be treated as proof of browser WebRTC p95/p99 until a waiver answer arrives or the missing credential/live-session environment is provided.
