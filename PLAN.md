# Plan: Complete WebRTC Transport Ownership Migration Out Of Hub

Ticket: `ticket_1777787330_803169` - Complete WebRTC transport ownership migration out of Hub.

## Intent

Finish the follow-up migration left by the workerized WebRTC project. The Hub
must stop owning WebRTC data-plane lifecycle, crypto, ratchet trigger
detection, stale-offer bookkeeping, receiver polling, and backpressure recovery
dispatch. Those responsibilities move into typed
`WebRtcPeerRegistry` / `WebRtcTransportRunner` APIs.

The Hub remains the central orchestrator. It keeps auth, pairing, Rails
signaling coordination, capability issuance, Lua callbacks, terminal attach
policy, session authorization, and summarized transport event handling. It
should receive typed results and requests from the WebRTC transport boundary,
not reach into per-peer channels, sender queues, generation maps, or raw
transport receivers.

This is a cold-turkey ownership cleanup. Do not preserve parallel Hub-owned and
registry-owned paths for the same WebRTC responsibility.

## Hard Constraints

- Repo instructions require vault bootstrap and CLI tests through
  `cd cli && ./test.sh ...`, not raw `cargo test`.
- Vault convention: Hub is the central orchestrator, workers/adapters own
  data-plane mechanics and report typed summaries back to Hub.
- Vault convention: cold-turkey migrations eliminate ambiguous dual paths.
- `docs/worker-actor-contracts.md` already says `WebRtcPeerRegistry` is the
  only holder of per-browser `WebRtcChannel` instances. The implementation must
  match that claim or the claim must be narrowed exactly.
- Rails/browser UI is not in scope. No Catalyst, Elements, Hotwire, or
  `tmp/tailwind_plus_preview` work is needed unless implementation unexpectedly
  changes browser UI.

## Coordination

Sibling ticket: `ticket_1777787620_100658` - Wire browser WebRTC traffic
through ClientWorker instead of adapter-only conversion.

Shared touch points:

- `cli/src/worker/webrtc.rs`
  - `WebRtcTransportRunner`
  - `WebRtcTransportAdapter`
  - `WebRtcPeerRegistry::ingress_to_client`
- `cli/src/worker/client.rs`
  - `ClientWorkerMessage`
  - `ClientControlFrame`
  - browser subscribe/input/control routing once the sibling ticket lands
- `cli/src/worker/transport.rs`
  - `TransportIngress`
  - `TransportEgress`
- `cli/src/worker/hub_control.rs`
  - `TransportPeerStateChanged`
  - `TransportSignalReady`
  - `TransportBackpressure`
  - `TransportRatchetRestartRequested`

Contract that survives either merge order:

- This ticket owns WebRTC peer lifecycle, offer/answer, generation, pending
  ICE, ratchet trigger, queue, and recovery ownership boundaries.
- `ticket_1777787620_100658` owns routing browser application traffic through
  long-lived `ClientWorker` mailboxes.
- This ticket must not add a second browser-worker routing path. Where a
  message is ready for ClientWorker but the sibling ticket has not landed, keep
  the existing Hub/Lua dispatch as an explicitly named temporary caller of the
  typed `WebRtcTransportRunner` ingress result. Do not add new raw JSON or
  per-peer sender maps in Hub.
- After both tickets land, WebRTC ingress should flow:

```text
DataChannel -> WebRtcChannel -> WebRtcPeerRegistry/WebRtcTransportRunner
  -> WebRtcTransportAdapter -> ClientWorkerMessage -> ClientWorker -> Hub policy/Lua as needed
```

## Current Ownership To Remove

Remove or replace these Hub-owned WebRTC data-plane symbols:

- `cli/src/hub/server_comms.rs`
  - `handle_webrtc_offer`
  - `HubEvent::WebRtcOfferCompleted` handling that owns generation checks and a
    returned `WebRtcChannel`
  - `handle_webrtc_message`
  - `try_ratchet_restart`
  - `send_ratchet_restart` transport delivery logic
  - `poll_webrtc_peer_messages`
  - `send_backpressure_recovery_snapshots`
  - WebRTC receiver polling with `take_pty_input_rx` / `restore_pty_input_rx`
  - WebRTC receiver polling with `take_stream_frame_rx` /
    `restore_stream_frame_rx`
  - WebRTC receiver polling with `take_pty_output_rx` /
    `restore_pty_output_rx`
  - WebRTC receiver polling with `take_outgoing_signal_rx` /
    `restore_outgoing_signal_rx`
- `cli/src/hub/run.rs`
  - long-lived raw WebRTC receiver ownership taken from `hub.webrtc` at run
    startup and restored at shutdown.
- `cli/src/hub/events.rs`
  - `HubEvent::WebRtcOfferCompleted` carrying `WebRtcChannel` back to Hub.
- `cli/src/worker/webrtc.rs`
  - public `take_*_rx` / `restore_*_rx` production APIs that make Hub the
    receiver owner.
  - scaffold comments saying registry/runner owns production lifecycle before
    the code actually does.
- `docs/worker-actor-contracts.md`
  - any claim stronger than the landed code.

## Seven Drift Items

### 1. Offer Generation And Answer Encryption

Decision: move.

Current Hub ownership:

- `Hub::handle_webrtc_offer` builds `WebRtcChannel`, inserts/removes it from
  `self.webrtc`, increments generation, spawns negotiation, encrypts the
  answer, and sends `HubEvent::WebRtcOfferCompleted`.
- `HubEvent::WebRtcOfferCompleted` carries `WebRtcChannel` back into Hub for
  stale generation checks and re-insertion.

Destination:

- Add `WebRtcPeerRegistry::start_offer(WebRtcOfferRequest) ->
  WebRtcOfferStart`.
- Add `WebRtcPeerRegistry::complete_offer(WebRtcOfferCompletion) ->
  WebRtcOfferCompletionOutcome`.
- Add `WebRtcTransportRunner::negotiate_offer(...)` for async SDP answer
  generation and answer encryption.
- Store the in-flight channel inside registry state keyed by
  `{browser_identity, generation}`. Hub must not remove or reinsert channels.

Typed return surface:

- `HubControlMessage::TransportPeerStateChanged` for connecting/failed states.
- `HubControlMessage::TransportSignalReady` with
  `TransportSignal::Answer { browser_identity, envelope }` once encrypted.

Implementation detail:

- Pass the existing `CryptoService` handle into the offer request so answer
  encryption happens in worker/webrtc, not Hub.
- `serde_json::Value` remains allowed only inside `TransportSignal::Answer`
  because it is the already serialized Olm relay envelope.

### 2. Decrypt-Failure Ratchet Trigger Detection

Decision: move trigger/dedup; keep fresh bundle generation in Hub policy.

Current Hub ownership:

- `Hub::poll_webrtc_peer_messages` receives `ChannelError::DecryptionFailed`.
- `Hub::try_ratchet_restart` deduplicates by Olm key/tab id.
- `Hub::send_ratchet_restart` both generates bundle bytes and queues
  DataChannel delivery.

Destination:

- Add `WebRtcPeerRegistry::record_decrypt_failure(browser_identity) ->
  Option<TransportRatchetRestartRequested>`.
- Move Olm-key/tab-id dedup state from Hub into registry.
- Keep bundle generation in Hub because it mutates trusted crypto policy.
- Add `WebRtcPeerRegistry::queue_bundle_refresh(browser_identity, bundle_bytes)`
  for DataChannel delivery.

Typed return surface:

- `HubControlMessage::TransportRatchetRestartRequested`.
- Hub handles that by generating the bundle and emitting ActionCable relay as
  before, then queues the DataChannel bundle through the registry method.

### 3. DataChannel Close/Open Lifecycle Summaries

Decision: move.

Current Hub ownership:

- Hub cleanup paths infer connection state from channel maps and call Lua
  callbacks while send tasks may still own per-peer state.

Destination:

- `WebRtcTransportRunner` owns DataChannel open/close/error events.
- `WebRtcPeerRegistry::mark_data_channel_open(browser_identity, generation)`.
- `WebRtcPeerRegistry::mark_data_channel_closed(browser_identity, generation,
  reason)`.
- Registry aborts peer send/ping tasks, drops queues, records close waiters,
  and returns one cleanup summary.

Typed return surface:

- `HubControlMessage::TransportPeerStateChanged` with:
  - `Connected { generation, mode }`
  - `Disconnected { generation, reason: DataChannelClose | DataChannelError |
    SendTimeout | ReplacedByNewPeer | ExplicitDisconnect }`

Hub may translate those summaries to Lua `peer_connected` /
`peer_disconnected` callbacks. Hub must not own DataChannel event handlers.

### 4. Stale Offer Completion Rejection

Decision: move.

Current Hub ownership:

- `HubEvent::WebRtcOfferCompleted` branch checks
  `self.webrtc.current_offer_generation`, disconnects stale channels, and
  clears offer state on failure.

Destination:

- `WebRtcPeerRegistry::complete_offer` checks generation and owns stale channel
  disconnect.
- Stale completion returns `WebRtcOfferCompletionOutcome::StaleDropped`.
- Failed completion returns `WebRtcOfferCompletionOutcome::FailedCleaned`.

Typed return surface:

- No Hub event should carry a `WebRtcChannel`.
- Hub receives either no-op stale summary for metrics/logging or a typed failed
  peer state summary.

### 5. Pending ICE Generation Gating

Decision: move.

Current Hub ownership:

- Hub drains pending ICE from registry after answer emission and calls
  `self.webrtc.apply_pending_ice_candidates(...)`.
- Hub logs stale queued candidate drops.

Destination:

- `WebRtcPeerRegistry::queue_or_apply_ice(browser_identity, candidate)` handles
  current-generation tagging and max queue length.
- `WebRtcPeerRegistry::apply_queued_ice_for_offer(browser_identity,
  offer_generation)` applies only matching generation candidates after answer
  emission.

Typed return surface:

- ICE relay remains `TransportSignalReady::Ice`.
- ICE apply failures are registry-owned diagnostics/metrics, not Hub channel
  access.

Invariant:

- Preserve existing browser answer-first behavior: send answer before applying
  queued ICE so slow or invalid candidates cannot delay answer relay.

### 6. Recovery Snapshot Dispatch Boundaries

Decision: move dispatch mechanics; Hub supplies authorized session snapshots.

Current Hub ownership:

- `Hub::send_backpressure_recovery_snapshots` drains registry recovery entries,
  takes per-peer senders, fetches session snapshots, prepares gzip payloads,
  sends directly to peer queue, and records sent/empty/failed counters.

Destination:

- Registry owns recovery entry cooldown, peer sender capacity checks, sender
  retention, and final `WebRtcAdapterCommand::Pty` dispatch.
- Add `WebRtcPeerRegistry::drain_recovery_requests(now) ->
  Vec<WebRtcRecoverySnapshotRequest>`.
- Add `WebRtcPeerRegistry::complete_recovery_snapshot(request_id,
  WebRtcRecoverySnapshotPayload)` for successful/empty/failed provider results.
- Add `WebRtcTransportRunner::prepare_recovery_payload(snapshot)` or a
  registry helper using existing `worker::session_io::timed_prepare_snapshot_payload`.

Hub role:

- Hub validates session ownership and supplies snapshot bytes via a narrow
  callback/request response because `handle_cache` and session policy remain
  Hub-owned.
- Hub does not take peer senders or send recovery payloads directly.

Typed return surface:

- Existing `HubControlMessage::TransportBackpressure` for queue pressure.
- Add or reuse a typed request shape such as
  `HubControlMessage::TransportBackpressure { pressure }` plus registry-local
  recovery request queue. Do not encode recovery snapshot requests as raw JSON.

Queue decisions:

- `WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY`: 2048.
- `WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY`: 512.
- `WEBRTC_STREAM_FRAME_QUEUE_CAPACITY`: 1024.
- `WEBRTC_PTY_INPUT_QUEUE_CAPACITY`: 2048.
- `WEBRTC_FILE_INPUT_QUEUE_CAPACITY`: 128.
- `PEER_SEND_CHANNEL_CAPACITY`: 256.
- `PEER_SEND_TIMEOUT`: 2 seconds.
- `BACKPRESSURE_SNAPSHOT_COOLDOWN`: 500 ms.

### 7. Misleading Scaffold-Only Ownership Claims

Decision: replace with exact post-migration wording.

Claims to update:

- `docs/worker-actor-contracts.md`
  - Current wording says `WebRtcPeerRegistry` is the only holder of
    per-browser `WebRtcChannel` instances and that DataChannel crypto is
    adapter/registry concern.
  - After this ticket, keep this as canonical only if implemented. If any
    handshake exception remains, replace the crypto paragraph with:

```text
Handshake-time SDP answer encryption remains Hub-owned only if the Hub never
owns a WebRtcChannel, generation map, pending ICE queue, or DataChannel sender.
All DataChannel crypto, ratchet trigger detection, peer lifecycle, pending ICE
application, and recovery dispatch are WebRtcPeerRegistry/WebRtcTransportRunner
responsibilities.
```

- `cli/src/worker/webrtc.rs`
  - Replace “production hub creates and drives runners” comments once registry
    owns offer completion and lifecycle.
  - Remove or narrow comments implying production traffic enters runner if
    `ticket_1777787620_100658` has not landed.
- `PLAN.md`
  - This file replaces the stale prior-ticket plan and is the authoritative
    plan for `ticket_1777787330_803169`.

## File-Level Implementation Sequence

1. Add typed request/outcome structs in `cli/src/worker/webrtc.rs`:
   - `WebRtcOfferRequest`
   - `WebRtcOfferStart`
   - `WebRtcOfferCompletion`
   - `WebRtcOfferCompletionOutcome`
   - `WebRtcIngressOutcome`
   - `WebRtcRecoverySnapshotRequest`
   - `WebRtcRecoverySnapshotResult`
2. Move offer channel creation from `Hub::handle_webrtc_offer` into
   `WebRtcPeerRegistry::start_offer`.
3. Move bounded replacement close wait into registry:
   `WebRtcPeerRegistry::wait_for_replaced_peer_close(olm_key, 100ms)`.
   This method must never block the Hub event loop longer than the bounded
   wait.
4. Move async SDP negotiation and answer encryption into
   `WebRtcTransportRunner::negotiate_offer`.
5. Replace `HubEvent::WebRtcOfferCompleted { channel, ... }` with either:
   - `HubEvent::WebRtcOfferCompleted { browser_identity, offer_generation,
     encrypted_answer }` plus registry-owned completion, or
   - a direct `HubControlMessage::TransportSignalReady` result from the
     registry task.
   Preferred: direct typed transport control result with no Hub event carrying
   offer state.
6. Move stale completion and failure cleanup into
   `WebRtcPeerRegistry::complete_offer`.
7. Replace Hub ICE queue/drain/apply calls with
   `WebRtcPeerRegistry::queue_or_apply_ice` and
   `apply_queued_ice_for_offer`.
8. Move `handle_webrtc_message` JSON parse and adapter conversion into
   `WebRtcTransportRunner::handle_plaintext_payload` returning
   `WebRtcIngressOutcome`:
   - `PongQueued`
   - `TerminalColorProfile(serde_json::Value)`
   - `LuaMessage(serde_json::Value)`
   - `ClientWorker(ClientWorkerMessage)`
   - `RatchetRestartRequested`
   Hub may still call Lua or terminal color profile handlers from the typed
   outcome.
9. Move ratchet trigger dedup into registry. Keep Hub bundle generation but
   queue DataChannel bundle refresh via registry.
10. Move backpressure recovery dispatch mechanics into registry. Hub supplies
    session snapshot bytes only through a typed request/result boundary.
11. Replace production `take_*_rx` / `restore_*_rx` use with registry-owned
    forwarding/drain methods:
    - `drain_pty_inputs`
    - `drain_file_inputs`
    - `drain_stream_frames`
    - `drain_outgoing_signals`
    - `drain_pty_outputs`
    These may internally own receivers but must not expose receiver ownership
    to Hub. Existing cfg(test) helpers can remain only if named test-only.
12. Update docs/comments listed above.
13. Remove stale Hub methods or reduce them to policy wrappers with names that
    do not claim transport ownership.

## Regression Tests

Add focused tests primarily in `cli/src/worker/webrtc.rs` under `#[cfg(test)]`.
Use `cli/src/hub/server_comms.rs` tests only for Hub policy translation.

Required tests:

- DataChannel close after Connected
  - Arrange a registry peer marked `Connected`.
  - Simulate DataChannel close for the same generation.
  - Assert one `TransportPeerStateChanged::Disconnected` summary with
    `DataChannelClose`.
  - Assert peer send task/ping task/recovery entries are removed.
  - Assert duplicate close for same generation is ignored.
- Quick successive offers with stale completion rejection
  - Start offer generation 1, then generation 2 for same browser.
  - Complete generation 1 after generation 2 starts.
  - Assert stale completion is dropped, old channel is disconnected, and no
    answer signal is emitted.
  - Complete generation 2 and assert answer signal is emitted once.
- Bounded replacement close wait
  - Register stale same-device peer with a pending close receiver that does not
    resolve.
  - Start replacement offer.
  - Assert elapsed wait is bounded to the configured 100 ms window plus test
    tolerance.
  - Assert replacement proceeds and stale peer cleanup reason is
    `ReplacedByNewPeer`.

Additional tests:

- Pending ICE generation gating drops old-generation queued candidates and
  applies only current-generation candidates after answer emission.
- Decrypt failure ratchet trigger deduplicates by Olm key and tab id and emits
  only one `TransportRatchetRestartRequested`.
- Backpressure recovery dispatch:
  - cooled entry with full peer queue remains queued or records failed without
    Hub direct send
  - empty snapshot records `snapshot.backpressure_recovery.empty`
  - successful snapshot queues one `WebRtcAdapterCommand::Pty`
- Ingress handling returns typed outcomes for `dc_ping`, `dc_pong`,
  `terminal_color_profile`, Lua JSON fallback, and decrypt-failure restart.

## Verification

Run CLI tests through the wrapper:

```bash
cd cli && ./test.sh --unit worker
cd cli && ./test.sh --unit webrtc
cd cli && ./test.sh --unit server_comms
```

If touched code crosses hub runtime wiring, also run:

```bash
cd cli && ./test.sh --unit hub
```

Static ownership checks after implementation:

```bash
rg -n "fn handle_webrtc_offer|WebRtcOfferCompleted|fn handle_webrtc_message|fn try_ratchet_restart|fn send_ratchet_restart|fn poll_webrtc_peer_messages|fn send_backpressure_recovery_snapshots" cli/src/hub
rg -n "take_.*_rx|restore_.*_rx" cli/src/hub cli/src/hub/run.rs
rg -n "webrtc_pending_ice_candidates|webrtc_offer_generation|webrtc_backpressure_recovery" cli/src/hub
rg -n "remove_channel\\(|insert_channel\\(|take_pending_close_for_olm\\(|apply_pending_ice_candidates\\(" cli/src/hub
```

Expected result: no matches in Hub-owned production code. If a test-only match
remains, it must be behind `#[cfg(test)]` and named as a test helper, not a
production ownership path.

Wire protocol check:

- `docs/webrtc-protocol.md` should not need changes because browser wire frames
  stay unchanged.
- Rails channel/system tests are not required unless the ActionCable envelope
  shape changes. This plan preserves the envelope shape.

## Done Criteria

- `PLAN.md` is this ticket's plan and no stale prior-ticket plan remains.
- Hub no longer owns the seven drift items above.
- Registry/runner APIs own lifecycle, crypto handoff, ratchet trigger, pending
  ICE, close/open summaries, receiver drains, and recovery dispatch.
- Hub receives typed summaries/requests and keeps policy authority only.
- Regression tests cover the three ticket-required scenarios.
- Static `rg` checks prove removed Hub symbols and raw receiver ownership are
  gone from production Hub code.
- Docs/comments no longer claim aspirational worker ownership ahead of code.
