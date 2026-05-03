# Plan: Move WebRTC Onto Client Worker Transport Adapter

Ticket: `ticket_1777747015_303075` - Move WebRTC Onto Client Worker Transport Adapter.

## Goal

Move browser WebRTC peer lifecycle and the DataChannel hot path out of the Hub
control loop and behind a concrete ClientWorker transport adapter. The Hub must
remain responsible for auth, pairing, signaling coordination, capability
issuance, attach/detach/reconnect/shutdown policy, and summarized typed
transport events.

This is a cold-turkey data-plane migration for WebRTC. Do not ship both
hub-resident and adapter-resident WebRTC send/lifecycle paths live at the same
time. The ambiguity itself is the risk here: split ownership between Hub maps
and adapter state is exactly what causes stale peers, duplicate reconnect
decisions, and send-to-unknown-peer noise.

## Hard Constraints

- Hub is the central orchestrator. Workers and adapters request policy changes
  through typed messages; they do not mutate Hub registries directly.
- Client workers own transport-neutral stream state. Browser, TUI, and socket
  clients must continue to share the same worker contract shape.
- Rails ActionCable channels remain opaque relays. `HubSignalingChannel` and
  `HubCommandChannel` must not inspect decrypted signaling content.
- WebRTC signaling continues through the existing Lua primitive into one Rust
  hub router. The Hub validates pairing/auth/capability context, then delegates
  async peer work to the adapter runner.
- Olm encryption remains mandatory. Use the existing `CryptoService =
  Arc<Mutex<VodozemacCrypto>>` path from `cli/src/relay/crypto_service.rs` and
  `cli/src/relay/olm_crypto.rs`; no plaintext DataChannel fallback.
- Hot-path queues must be bounded, deterministic, and report typed
  `WorkerBackpressure`.
- CLI tests must use `cd cli && ./test.sh ...`, never raw `cargo test`.
- No UI work is planned. `tmp/tailwind_plus_preview` is absent in this
  worktree. If browser UI becomes necessary later, use existing React/Vite
  Catalyst components and `IconGlyph`, not raw HTML, unicode glyphs, or new
  styling primitives.

## Current Ownership To Move

Move or replace these Hub-owned WebRTC data-plane fields and methods:

- [cli/src/hub/mod.rs](cli/src/hub/mod.rs)
  - `webrtc_channels: HashMap<String, WebRtcChannel>`
  - `webrtc_send_tasks: HashMap<String, PeerSendState>`
  - `dc_ping_tasks`
  - `webrtc_pending_closes`
  - `webrtc_offer_generation`
  - `webrtc_pending_ice_candidates`
  - `webrtc_pty_output_tx/rx`
  - `stream_frame_tx/rx`, `pty_input_tx/rx`, `file_input_tx/rx`
  - `webrtc_backpressure_recovery`
- [cli/src/hub/server_comms.rs](cli/src/hub/server_comms.rs)
  - `cleanup_disconnected_webrtc_channels`
  - `cleanup_webrtc_channel`
  - `spawn_peer_send_task`, `try_send_to_peer`, `send_webrtc_raw`
  - `spawn_dc_ping_task`
  - WebRTC portions of `handle_webrtc_offer`, `WebRtcOfferCompleted`,
    `handle_webrtc_pty_output_batch`, `poll_webrtc_pty_output`,
    `handle_pty_input`, stream-frame polling, and file-input polling.
- [cli/src/channel/webrtc.rs](cli/src/channel/webrtc.rs)
  - Keep reusable WebRTC primitives, but extract adapter-runner friendly APIs
    from `WebRtcChannel`/`WebRtcSender` instead of keeping Hub-owned maps and
    send tasks.

## Adapter Shape

Add a concrete adapter runner, not async methods on the existing sync trait.

Files:

- Add `cli/src/worker/webrtc.rs`.
- Export it from `cli/src/worker/mod.rs`.
- Extend `cli/src/worker/transport.rs` only for typed frame variants if needed;
  do not make the existing `TransportAdapter` trait async.

Types:

- `WebRtcTransportAdapter`
  - Implements the existing synchronous `TransportAdapter`.
  - Converts decoded transport frames into `ClientWorkerMessage`.
  - Converts `TransportEgress` into adapter-local `WebRtcAdapterCommand`
    values, not direct socket sends.
- `WebRtcTransportRunner`
  - Owns async peer state for one browser identity.
  - Spawned by Hub after auth/pairing/capability checks.
  - Holds `ClientWorkerHandle`, `CryptoService`, signaling relay sender,
    bounded outbound command receiver, and WebRTC channel state.
- `WebRtcPeerRegistry`
  - Adapter-owned registry keyed by browser identity.
  - Replaces Hub's `webrtc_channels`, `webrtc_send_tasks`,
    `dc_ping_tasks`, `webrtc_pending_closes`, offer generation, and pending ICE
    maps.
  - Exposes typed methods to Hub for `offer`, `ice`, `disconnect`, and
    `shutdown_peer`.

Channel wiring:

- Browser DataChannel ingress -> `WebRtcTransportRunner` decrypts/decodes ->
  `WebRtcTransportAdapter::ingress_to_client` ->
  `ClientWorkerHandle.try_send`.
- ClientWorker egress -> bounded adapter command queue ->
  `WebRtcTransportRunner` encrypts/compresses/sends on DataChannel.
- Adapter summary events -> `HubControlMessage` through
  `hub_control_tx`.
- Signaling relay output -> existing ActionCable command channel path.

The existing sync `TransportAdapter` remains a pure conversion boundary. The
async runner owns peer connection lifecycle, ICE callbacks, DataChannel events,
queue draining, crypto, liveness probes, and cleanup.

## Typed Hub-Control Surface

Extend [cli/src/worker/hub_control.rs](cli/src/worker/hub_control.rs) with
typed WebRTC summaries. Avoid new `serde_json::Value` control surfaces.

Add:

- `TransportPeerState`
  - `Connecting { generation: u64 }`
  - `Connected { generation: u64, mode: TransportConnectionMode }`
  - `Disconnected { generation: u64, reason: TransportDisconnectReason }`
  - `Failed { generation: u64, reason: TransportDisconnectReason }`
- `TransportConnectionMode`
  - `Unknown`
  - `Direct`
  - `Relayed`
- `TransportDisconnectReason`
  - `DataChannelClose`
  - `DataChannelError`
  - `ConnectionTimeout`
  - `SendTimeout`
  - `MissedLivenessProbes`
  - `ReplacedByNewPeer`
  - `ExplicitDisconnect`
  - `Shutdown`
- `TransportSignal`
  - `Ice { browser_identity: String, envelope: serde_json::Value }`
  - `Answer { browser_identity: String, envelope: serde_json::Value }`
  Signaling envelopes stay opaque because they are already serialized
  `OlmEnvelope` values for Rails relay.

Add `HubControlMessage` variants:

- `TransportPeerStateChanged { client_id, browser_identity, state }`
- `TransportSignalReady { client_id, signal }`
- `TransportBackpressure { origin, pressure }`
- `TransportRatchetRestartRequested { client_id, browser_identity }`

Keep existing `AttachClient`, `DetachClient`, `Backpressure`, `Reconnect`, and
`Shutdown` for hub policy. The Hub may translate adapter summaries into Lua
callbacks and browser-visible status events, but new worker-to-hub surfaces must
be typed.

## Crypto Ownership

The adapter runner owns crypto operations for its peer.

- `WebRtcTransportRunner` holds `CryptoService` cloned from
  `hub.browser.crypto_service`.
- Signaling:
  - Incoming offer/ICE envelopes are decrypted before peer state machine work.
  - Answer/ICE relay envelopes are encrypted by the runner and emitted as
    `TransportSignalReady`.
- DataChannel:
  - `encrypt_binary` / `decrypt_binary` move into the adapter runner path.
  - Keep content bytes and wire constants from
    `cli/src/relay/olm_crypto.rs`: `CONTENT_MSG`, `CONTENT_PTY`,
    `CONTENT_STREAM`, `CONTENT_FILE`, `CONTENT_FILE_CHUNK`.
- Identity trust:
  - Continue deriving the peer Olm key from browser identity with
    `extract_olm_key`.
  - Do not accept a DataChannel message from a browser identity whose Olm key
    does not match the paired/trusted identity.
- Ratchet restart:
  - Preserve `MSG_TYPE_BUNDLE_REFRESH` type 2 bundle refresh.
  - Adapter sends bundle refresh over the DataChannel when Hub policy requests
    a fresh bundle or when decrypt failure thresholds require restart.
  - Hub keeps authority for capability issuance and fresh-bundle generation;
    adapter owns transport delivery of the resulting bundle bytes.

## Queue And Backpressure Policy

Use bounded queues only.

- ClientWorker mailbox: existing `CLIENT_WORKER_QUEUE` capacity `1024`.
- Adapter command queue: existing `TRANSPORT_ADAPTER_QUEUE` capacity `512`.
- Per-peer outbound send queue: keep current capacity `256`
  (`PEER_SEND_CHANNEL_CAPACITY`) unless tests show it is too small.
- Inbound PTY frames from browser: capacity `2048`.
- Inbound stream frames: capacity `1024`.
- Inbound file transfers: capacity `128`.

Frame policy:

- Terminal output (`CONTENT_PTY` output): `try_send`; drop on full and emit
  `WorkerBackpressure` with `client_id`, `session_uuid`, source, and capacity.
  Hub schedules one coalesced recovery snapshot per `{browser_identity,
  session_uuid}` after cooldown.
- Snapshots and process/control frames: reliable within bounded queue. If queue
  is full, emit `TransportBackpressure` and request reconnect instead of silent
  loss.
- Close/shutdown frames: best effort, then force runner cleanup and emit
  `TransportPeerStateChanged::Disconnected`.
- Stream frames: bounded `try_send`; drop data frames on full, but close/error
  frames prefer reconnect/cleanup.
- File input: reject/drop on full with typed backpressure; never block Hub.

Dead-peer detection:

- Send timeout: current `PEER_SEND_TIMEOUT` of 2 seconds marks peer dead and
  emits `Disconnected { reason: SendTimeout }`.
- Liveness probes: adapter sends `dc_ping` every 10 seconds. Three missed
  pongs or 30 seconds without liveness emits
  `Disconnected { reason: MissedLivenessProbes }`.
- DataChannel close/error immediately emits `DataChannelClose` or
  `DataChannelError` and starts cleanup.

## Failure Modes Addressed

### DataChannel closes after successful connect

Current risk: Hub records a peer as connected, then DataChannel close/error
cleanup races with send queues and Lua peer callbacks. The Hub may continue
to queue work to a peer whose DataChannel is already gone.

Mitigation:

- Adapter runner is the sole owner of DataChannel open/close/error handlers.
- Connected state is emitted only after DataChannel open, not merely ICE state.
- Close/error drops the adapter outbound sender, marks generation disconnected,
  aborts per-peer tasks, and emits exactly one typed disconnect summary.
- Hub cleanup becomes idempotent policy handling of the summary, not a second
  owner of socket state.

Regression tests:

- Adapter unit test: DataChannel close after `Connected` emits one
  `TransportPeerStateChanged::Disconnected(DataChannelClose)` and rejects
  later sends without unknown-peer logs.
- Hub unit/static test: no Hub send path can enqueue into `WebRtcSender`
  directly after migration.

### Reconnect bursts

Current risk: stale offer completions, pending ICE, close-complete waits, and
browser reconnect attempts are coordinated by multiple Hub maps and periodic
cleanup, creating bursty replacement behavior.

Mitigation:

- `WebRtcPeerRegistry` owns reconnect generation per browser identity.
- Every offer, answer completion, ICE candidate, and peer event carries the
  generation; stale completions and ICE are dropped.
- Previous connection close-complete waiting moves into the registry before a
  replacement runner starts.
- Direct-to-relay churn fallback stays in the adapter runner.

Regression tests:

- Two offers in quick succession: first async completion is ignored, second
  owns the peer.
- ICE that arrives during offer setup is queued and drained only for the
  current generation.
- Replacement waits for close completion or bounded timeout before new runner
  creation.

### Send-to-unknown-peer noise

Current risk: Hub `try_send_to_peer` sees no `PeerSendState` and logs repeated
unknown-peer bursts while stale Lua/control paths continue sending after peer
cleanup.

Mitigation:

- Hub no longer sends directly to per-peer WebRTC channels.
- Adapter registry owns peer lookup and returns typed `Disconnected` or
  `SendRejected` state to ClientWorker/Hub without repeated Hub log noise.
- Unknown/disconnected sends are coalesced per `{browser_identity, reason}`
  window and surfaced as metrics, not unbounded warnings.

Regression tests:

- After peer cleanup, repeated sends emit at most one coalesced metric/log in
  the window and do not call DataChannel send.
- Hub static check confirms `try_send_to_peer`/`webrtc_send_tasks` are removed
  or no longer used by WebRTC.

## Cleanup And Leak Prevention

`WebRtcPeerRegistry` becomes the owner of stale peer cleanup.

- Replace `cleanup_disconnected_webrtc_channels()` with registry cleanup:
  - scan adapter runners on a timer/tick owned by the adapter registry;
  - disconnect peers stuck in `Connecting` longer than 30 seconds;
  - call `PeerConnection::close`;
  - abort send, ping, offer, stream, and file tasks;
  - prune completed pending-close receivers every scan.
- Hub receives `TransportPeerStateChanged::Disconnected` and performs policy
  cleanup:
  - unregister terminal client peer;
  - drop pending terminal attach intents;
  - abort subscription/forwarder state owned by Hub if any remains;
  - notify Lua peer-disconnected exactly once.
- Add a static check that `webrtc_channels`, `webrtc_send_tasks`,
  `dc_ping_tasks`, and `webrtc_pending_closes` no longer live on `Hub`.

## Browser Contract

Default expectation: browser-visible wire and events remain byte-identical.

No browser JS files should need functional changes if the migration is correct:

- `app/frontend/lib/workers/bridge.js`
- `app/frontend/lib/transport/hub_peer_connection.js`
- `app/frontend/lib/transport/hub_peer_lifecycle.js`
- `app/frontend/lib/transport/hub_channel_protocol.js`
- `app/frontend/lib/transport/hub_signaling_client.js`
- `app/frontend/lib/connections/hub_route.js`
- `app/frontend/components/hub/SidebarConnectionStatus.jsx`
- `app/frontend/components/hub/ConnectionOverlay.jsx`

Preserve:

- ActionCable signaling envelope shape.
- DataChannel content frame bytes.
- `terminal_{session_uuid}` subscription IDs.
- `connection:state`, `connection:mode`, `connection:stalled`,
  `signaling:state`, `subscription:*`, `session:*`, `stream:frame`, and
  push-related events.

If implementation discovers a necessary browser-visible delta, stop and update
this plan before coding the UI/browser change. Keep existing wire naming; do
not introduce "v2" branding for this migration.

## File-Level Implementation Sequence

1. Worker contract extensions
   - Edit `cli/src/worker/hub_control.rs` for typed transport summaries.
   - Edit `cli/src/worker/transport.rs` only if `TransportIngress` or
     `TransportEgress` needs additional typed frame variants.
   - Keep `cli/src/worker/client.rs` transport-neutral.

2. WebRTC adapter module
   - Add `cli/src/worker/webrtc.rs`.
   - Implement `WebRtcTransportAdapter`, `WebRtcTransportRunner`, and
     `WebRtcPeerRegistry`.
   - Reuse `WebRtcChannel`/`WebRtcSender` helpers where possible; extract
     helper functions from `cli/src/channel/webrtc.rs` only when they become
     reusable by the runner.

3. Crypto and wire reuse
   - Reference `cli/src/relay/crypto_service.rs` and
     `cli/src/relay/olm_crypto.rs` directly.
   - Keep type 2 `MSG_TYPE_BUNDLE_REFRESH` intact.
   - Move DataChannel encrypt/decrypt calls into the adapter runner.

4. Hub integration
   - Edit `cli/src/hub/mod.rs` to replace WebRTC maps with one registry handle.
   - Edit `cli/src/hub/server_comms.rs` so signaling handlers delegate to the
     registry and consume typed hub-control summaries.
   - Keep auth, pairing, capability, Lua callback, and ActionCable relay
     policy in Hub.

5. Remove old paths
   - Delete or fully disconnect old Hub send-task and cleanup functions:
     `spawn_peer_send_task`, `try_send_to_peer`, `send_webrtc_raw`,
     `cleanup_disconnected_webrtc_channels`, and direct
     `webrtc_channels` access.
   - Remove stale fields from `Hub`.

6. Tests and docs
   - Add focused Rust tests beside `cli/src/worker/webrtc.rs`.
   - Update existing `cli/src/hub/server_comms.rs` tests for the new boundary.
   - Update any worker contract docs if present.

## Verification Commands

Run focused CLI tests through the wrapper:

```bash
cd cli && ./test.sh --unit worker
cd cli && ./test.sh --unit webrtc
cd cli && ./test.sh --unit hub
```

If `cli/test.sh` does not support those filters exactly, use the nearest
supported filter and record the exact command in the implementation report.

Run static checks proving the old Hub-owned data path is gone:

```bash
rg -n "webrtc_channels|webrtc_send_tasks|dc_ping_tasks|webrtc_pending_closes" cli/src/hub
rg -n "spawn_peer_send_task|try_send_to_peer|send_webrtc_raw|cleanup_disconnected_webrtc_channels" cli/src/hub
rg -n "WebRtcSender|send_pty_raw|send_stream_raw|send_bundle_refresh" cli/src/hub
rg -n "TransportPeerStateChanged|TransportSignalReady|TransportRatchetRestartRequested|WebRtcTransportRunner|WebRtcPeerRegistry" cli/src
rg -n "self\\.webrtc\\.channel\\(|\\.channel_ids\\(|pub\\(crate\\) fn channel\\(|fn channel_ids\\(" cli/src/hub cli/src/worker/webrtc.rs
```

`WebRtcTransportRunner` and `WebRtcTransportAdapter` must also compile without
dead-code warnings in the filtered test output; warning-free `./test.sh --unit
worker`/`webrtc` output is part of the implementation evidence.

Run browser compatibility tests only if browser-visible events or frame shapes
change:

```bash
npx vitest run app/frontend/test/hub-peer-connection-peer-lost.test.js
npx vitest run app/frontend/test/hub-signaling-client.test.js
npx vitest run app/frontend/test/hub-connection-status.test.js
npx vitest run app/frontend/test/sidebar-connection-status.test.jsx
```

Rails channel behavior should remain unchanged. If signaling payload shape
changes unexpectedly, run:

```bash
bin/rails test test/channels/hub_signaling_channel_test.rb
bin/rails test test/controllers/hubs/webrtc_controller_test.rb
```

## Acceptance Checklist

- WebRTC peer lifecycle, ICE, DataChannel open/close/error, crypto
  encrypt/decrypt, liveness probes, outbound queue, backpressure, and
  dead-peer detection are adapter-owned.
- Hub no longer owns per-peer WebRTC channel/send-task/close maps.
- Hub receives typed summaries and keeps auth/pairing/signaling/capability
  policy.
- Rails ActionCable relay remains opaque.
- Browser wire/events stay byte-identical, or a deliberate contract update is
  documented before implementation.
- The three named failure modes have regression coverage.
- All hot-path queues are bounded and report deterministic typed
  backpressure.
