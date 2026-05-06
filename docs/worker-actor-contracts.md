# Worker Actor Contracts

Botster's workerized runtime boundary starts with typed Rust contracts. The
session I/O worker is the production read path for durable session-process PTY
output and the executable mailbox for session-scoped PTY input, paste/drop
writes, snapshot preparation, resize, mode/screen queries, color profile
updates, and shutdown requests. WebRTC peer/channel state lives behind
`WebRtcPeerRegistry`; browser terminal controls that have stable Botster-owned
shapes cross the transport adapter into `ClientWorkerMessage`, while Lua,
plugin, and relay-owned JSON remains an explicit boundary exception.

## Ownership

The hub remains the orchestration owner. Workers may request attach, detach,
reconnect, shutdown, lifecycle, and backpressure handling through
`worker::hub_control::HubControlMessage`, but they do not mutate hub state
directly.

Client workers are transport-neutral. They own a per-client stream of session
subscriptions, liveness state, outbound terminal/control messages, and PTY
input routing using `worker::client::ClientWorkerMessage`. They identify
clients with the existing `ClientId` type so browser, TUI, socket, and internal
clients stay equal.

Transport adapters are the only transport-specific boundary. A WebRTC adapter,
TUI adapter, socket adapter, or future adapter converts its local framing into
`TransportIngress` and converts generic worker output into `TransportEgress`.
Session I/O code and session processes must not import browser or WebRTC
concepts.

Session I/O workers mirror the current per-session process protocol. The
current production mailbox work is PTY input, resize, snapshot requests,
mode-flag requests, plain-screen requests, color profile updates, authorized
file paste/drop payloads, prepared snapshot payloads, and clean shutdown
requests. The worker emits structured terminal events and process-exit messages
back to the hub. The Unix socket wire protocol in `cli/src/session/protocol.rs`
remains the durable process boundary.

Plugin workers own plugin execution. The hub may keep descriptor registries for
discovery, routing, and UI state, but executable plugin code should be invoked
through a per-plugin bounded mailbox, keyed by a stable `PluginHandlerRef`
rather than by a Lua closure stored in hub state. Reloading a plugin means
replacing that plugin's worker and republishing its descriptors; unloading a
plugin means stopping that worker and removing only capabilities owned by that
plugin key. A slow or broken plugin must be able to saturate or kill only its
own worker, not the hub event loop, client workers, or session I/O workers.

## Session I/O Worker Runtime

`SessionConnection::install_reader()` installs one `SessionIoWorker` per active
session connection. The worker owns the blocking read side of that connection's
Unix socket and exits with that connection; reconnect creates a fresh
`SessionConnection` and a fresh worker after the hub's generation checks pass.

The worker keeps the session process protocol unchanged. `SessionConnection`
still owns synchronous RPC methods, while the worker decodes every socket frame
and routes snapshot, screen, mode-flags, and other control responses back to the
existing `response_rx` channel. A companion request processor owns bounded
`SessionIoRequest` mailbox work such as PTY input, paste path injection, and
prepared snapshot payloads. This avoids socket read contention without moving
hub orchestration policy into the worker.

File paste/drop data-plane mechanics live in `worker::session_io` behind the
per-session `SessionIoRequest` mailbox. The hub still authorizes the target
session and transport capabilities, then enqueues the already-authorized
filename and bytes as `SessionIoRequest::PasteFile`. The Session I/O worker
writes the file, injects the resulting path into the PTY, and reports
`SessionIoEvent::PasteFileWritten` or `SessionIoEvent::PasteFileFailed` back
through `HubEvent::SessionIo`. Paste temp paths are session-scoped and resolve
in this order: session manifest
`worktree_path` under `.botster/pastes/<session_uuid>`, Botster data dir under
`pastes/<session_uuid>`, then the OS temp dir under
`botster/pastes/<session_uuid>`. Cleanup is keyed by session UUID, never label,
and runs on real process exit plus hub drop.

Snapshot payload preparation also lives behind the session I/O mailbox. The hub
remains responsible for deciding when a snapshot is needed and where it should
be routed, but the worker owns snapshot prefixing and gzip preparation through
`SessionIoRequest::PrepareSnapshot` and `SessionIoEvent::PreparedSnapshot`.
`request_id` is an opaque correlation key scoped to the session; callers use it
to map prepared output back to peer/subscription or Lua refresh state. Browser
identities and WebRTC structs do not enter the worker contract.

PTY output does not cross the hub as a hot-path event. The session I/O worker
preserves byte order and publishes terminal bytes to subscribed client workers;
the hub only owns attach policy, pending snapshot correlation, lifecycle
events, and slow-path plugin/control decisions. Session I/O still uses short
coalescing windows before fan-out so subscribers receive bounded chunks instead
of one delivery per `FRAME_PTY_OUTPUT`. Coalescing flushes when any of these
boundaries is reached:

- 32 KiB of buffered output
- 16 output frames
- 4 ms since the first buffered output or metadata update
- an ordered structured event such as prompt mark, bell, notification, or
  process exit
- EOF, protocol desync, or worker shutdown

Sparse terminal metadata is coalesced in the same short window before delivery:
mode fields merge with last value winning, title and CWD keep the last value,
and prompt marks, bells, notifications, and process exits remain ordered
boundaries.

Browser, TUI, and socket clients receive terminal bytes, scrollback, process
exit, and terminal control frames through their transport-neutral
`ClientWorker` subscription path. Raw PTY bytes are not exposed as hub Lua
observer callbacks in the durable-session path; plugin code that needs stable
state should observe typed lifecycle/entity events or request explicit
snapshots.

## Data Flow

```text
transport-specific client
  -> TransportAdapter
  -> ClientWorkerMessage
  -> HubControlMessage or SessionIoRequest
  -> Hub-owned state or per-session process
```

Responses flow back through the same layers in reverse. The adapter owns
encoding and delivery details; the client worker owns client-scoped stream
state; the session I/O worker owns session-scoped PTY interaction; the hub owns
orchestration state and lifecycle policy.

That diagram is the terminal data-plane contract, not a promise that every
WebRTC JSON message enters the client worker. Hub-owned policy and synchronous
compatibility work still exists where the hub must decide routing, capability,
Lua callbacks, or RPC response correlation. Browser terminal
subscribe/unsubscribe/focus and binary PTY/file frames are the workerized path;
terminal color profile messages and unknown JSON are deliberately classified as
Lua/plugin/relay boundary traffic until they have stable typed worker frames.

The plugin execution contract follows the same shape, but for control-plane
extension code:

```text
hub-owned descriptor registry
  -> PluginHandlerRef
  -> PluginWorkerMessage
  -> per-plugin worker runtime
  -> PluginWorkerEvent
  -> hub-owned routing/state update
```

The hub should not pass `mlua::Function` values, raw closure pointers, or
plugin-owned mutable execution state across this boundary. Plugin registration
APIs may publish descriptors to hub-owned registries, but action handlers,
session actions, command handlers, hooks, event subscriptions, timer callbacks,
file watch callbacks, MCP handlers, surface route renderers, and asset message
handlers belong behind the plugin worker mailbox.

`lib.plugin_supervisor` is the Lua boundary for this path. Plugin-owned UI
actions, session actions, hub commands, hooks, event subscriptions, timer
callbacks, file watch callbacks, MCP tool/prompt/resource handlers, MCP proxy
auth-error handlers, surface route renderers, and plugin asset iframe message
handlers are loaded into the plugin worker VM and invoked by handler id through
`__plugin_worker_invoke`; built-in hub handlers remain local hub VM functions.
Hub-owned timer scheduling and MCP descriptor lists may keep descriptor state in
the hub, but plugin-owned timer callbacks, event callbacks, and MCP handlers
execute in the plugin worker. The plugin worker also pumps its local async HTTP,
WebSocket, and file watch callback queues so callbacks created inside
plugin-owned worker execution fire in that same worker. The hub-side closure
captured during descriptor publication is not the execution source for
plugin-owned handlers.

`worker::client::ClientWorker::start` is the executable core for this boundary.
It creates a bounded client mailbox, records subscriptions by session UUID,
emits hub-owned attach/detach/reconnect/shutdown/backpressure requests, routes
subscribed `SessionInput` frames to attached `SessionIoRequest::PtyInput`
mailboxes, and forwards subscribed session output/control frames to the
transport egress queue. TUI and local socket terminal streams are
production-wired through this actor: hub-owned Lua attach requests still decide
when a session exists, when pending attach intents resolve, and when subscription
tasks are cleaned up, but snapshot, live PTY output, process-exit, typed control,
plugin binary, and raw input traffic cross the client-worker boundary before a
TUI or socket adapter encodes them. WebRTC production terminal traffic enters
the same boundary through `worker::webrtc::WebRtcPeerRegistry` and
`WebRtcTransportRunner`; the adapter converts subscribe, unsubscribe,
focus-change, heartbeat, PTY input, and file input ingress into typed
`ClientWorkerMessage` or explicit hub-control messages. The hub may authorize
and correlate terminal attach work, but it must not handle durable-session PTY
bytes as hub events.

## WebRTC Transport Adapter

`WebRtcPeerRegistry` is the only holder of per-browser `WebRtcChannel`
instances. The Hub owns one registry handle and keeps auth, pairing,
capability, Rails relay coordination, Lua callbacks, terminal attach policy,
and summarized cleanup policy. WebRTC peer connection state, offer generation,
pending ICE queues, per-peer bounded send queues, DataChannel liveness pings,
unknown-peer burst coalescing, backpressure recovery tracking, and peer cleanup
bookkeeping stay inside the registry.

The registry also owns the WebRTC transport queue receivers. Production code
starts registry queue tasks for control-plane and transport-plane queues:
incoming browser frames, file ingress, outgoing signaling envelopes, stream
multiplexer frames, and peer liveness/control messages. Terminal payloads read
from those queues must enter `WebRtcTransportRunner` and the shared
`ClientWorker`/`SessionIoWorker` data-plane path; they must not be converted
into a hub hot-path PTY byte event. Hub code may handle typed control events,
but must not lease, take, restore, or poll raw WebRTC receivers.

The Hub must not recover the old escape hatch by reaching into a channel map or
`WebRtcSender` directly. New WebRTC work should add a typed registry method or
a typed adapter command instead. The expected production path is:

```text
browser DataChannel
  -> WebRtcChannel decrypt/decode
  -> WebRtcPeerRegistry
  -> WebRtcTransportRunner
  -> WebRtcTransportAdapter
  -> ClientWorkerMessage
  -> Hub-owned policy / Lua routing
```

Outbound WebRTC delivery follows the reverse ownership boundary. Hub policy
produces transport-neutral work or WebRTC adapter commands, then queues them
through `WebRtcPeerRegistry::queue_command`. The registry owns the bounded
per-peer command channel and the async send task that serializes terminal
bytes, JSON, stream, binary, and bundle-refresh frames onto the DataChannel.
Binary adapter commands must use the raw DataChannel send helper; JSON helpers
are only for JSON frames.

WebRTC transport summaries cross back to the Hub through typed
`HubControlMessage` variants:

- `TransportPeerStateChanged`
- `TransportSignalReady`
- `TransportBackpressure`
- `TransportRatchetRestartRequested`

`TransportSignal` envelopes intentionally carry `serde_json::Value` only at
the Rails relay boundary because those values are already serialized Olm
envelopes. Do not let that exception spread to new adapter control surfaces.

Crypto ownership is split by lifecycle phase. WebRTC offer mechanics run behind
`WebRtcPeerRegistry` / `WebRtcTransportRunner`, but handshake-time SDP answer
encryption is still hub-triggered because hub policy owns the authenticated
browser identity and crypto service. The hub starts negotiation with validated
policy inputs, `WebRtcTransportRunner::negotiate_offer` creates and encrypts
the answer, and completion returns as a typed hub event. DataChannel
encrypt/decrypt failure tracking, ratchet-trigger dedupe, and ratchet-delivery
transport are adapter/registry concerns. The Hub still generates fresh ratchet
bundles because that mutates trusted crypto policy, then queues the bytes
through `WebRtcPeerRegistry` instead of sending through a `WebRtcSender`
directly.

Regression coverage for this boundary should stay focused on the failure modes
that motivated the migration: a DataChannel closing after `Connected`, stale
offer completions during reconnect bursts, generation-scoped pending ICE, close
replacement waits, and coalesced send-to-unknown-peer noise. Use the repository
test wrapper, for example `cd cli && ./test.sh --unit worker`, rather than raw
`cargo test`.

Load and recovery verification for the workerized data plane should use
deterministic reproductions of observed daemon log shapes instead of live
`/tmp` log dependencies. Current coverage includes 1001-frame noisy PTY replay
with OSC title/cursor traffic, repeated session-reader EOF behavior,
generation-scoped WebRTC reconnect churn, cooled backpressure recovery sender
selection, and browser/TUI/socket parity through the same `ClientWorker` path.
These tests assert preserved byte order, bounded hub batch volume, stale
generation drops, and typed recovery state cleanup at the worker boundary.

`ClientWorkerConfig::session_io_txs` is only an initial seed path. Production
browser/TUI/socket terminal attach wiring registers per-session
`SessionIoRequest` senders with `ClientWorkerMessage::RegisterSessionIoSender`
before `SubscribeSession`, and detaches them with
`UnregisterSessionIoSender` when a terminal subscription stops, a WebRTC peer disconnects,
a session unregisters, or a process exits. The worker removes closed senders on
input delivery failures so stale session I/O channels do not keep accepting
client input.

Reconnect generation is tracked by the client worker. Frames wrapped with an
older generation are dropped before delivery, and reconnect health emits a
typed `HubControlMessage::Reconnect` so hub policy stays centralized. `Ping`
has an explicit observability response: the worker emits a transport-neutral
`TransportEgress::Pong { request_id }` with the original request ID.

Stable Botster-owned controls use typed variants at the worker contract. JSON
is reserved for boundary payloads whose shape is owned by Lua, plugins, or relay
protocols rather than the Rust worker contract. The canonical typed control set
includes:

- `ClientControlFrame::Pong` and `TransportEgress::Pong`
- `ClientControlFrame::TerminalAttach` and `TransportEgress::TerminalAttach`
- `ClientControlFrame::Snapshot` and `TransportEgress::Snapshot`
- `ClientControlFrame::ModeChanged` and `TransportEgress::ModeChanged`
- `ClientControlFrame::KittyChanged` and `TransportEgress::KittyChanged`
- `ClientControlFrame::FocusReportingChanged` and
  `TransportEgress::FocusReportingChanged`
- `ClientControlFrame::FocusChanged`, `TransportIngress::FocusChanged`, and
  `TransportEgress::FocusChanged`
- `ClientControlFrame::DcPong` and `TransportEgress::DcPong` for outbound
  heartbeat replies
- `ClientControlFrame::DcPongReceived` and `TransportIngress::DcPong` for
  inbound heartbeat acknowledgements, which are observations and must not echo
  back to the transport
- `ClientControlFrame::Scrollback`, `TransportEgress::Scrollback`,
  `ClientControlFrame::ProcessExited`, and `TransportEgress::ProcessExited`

`ClientControlFrame::BoundaryJson` and `TransportEgress::BoundaryJson` are
allowed only after a site is classified as a Lua/plugin/relay boundary. They
are not fallback paths for stable Botster-owned messages that happen to encode
as JSON on a socket, TUI, or WebRTC wire.

`TransportEgress` carries binary terminal payloads as typed frames, not JSON
shims. `TerminalBytes`, `Scrollback`, and `ProcessExited` all include the
session UUID; `Scrollback` additionally carries rows, columns, kitty keyboard
state, and opaque snapshot bytes. Socket adapters map these to
`Frame::PtyOutput`, `Frame::Scrollback`, and `Frame::ProcessExited`
losslessly. The TUI adapter maps the same egress to existing `TuiOutput`
variants without changing ratatui/crossterm rendering code. Plugin-level binary
messages use `TransportEgress::Binary` and stay outside PTY routing.

Subscription cleanup remains a hub policy boundary. `ProcessExited` control
frames are delivered only to subscribed clients; the client worker does not
auto-unsubscribe after delivery. The hub should send the matching
`UnsubscribeSession` or detach request when process lifecycle policy says the
client stream is over. `UnregisterSessionIoSender` is the canonical dynamic
cleanup request because the worker owns the active subscription ID and can emit
the matching `DetachClient`. If a client subscribes to the same session UUID
with the same subscription ID, the worker treats it as a no-op. If the
subscription ID changes, the worker emits `DetachClient` for the old ID before
emitting `AttachClient` for the replacement.

## Bounded Queues

Each worker contract exposes a `BoundedQueueConfig` constant:

- `HUB_CONTROL_QUEUE`
- `CLIENT_WORKER_QUEUE`
- `TRANSPORT_ADAPTER_QUEUE`
- `SESSION_IO_WORKER_QUEUE`

Follow-up implementation should use bounded `tokio::sync::mpsc` channels for
these mailboxes. Backpressure is a typed event, not an implicit shared flag.
This matches the existing hot-path observability work in
`docs/hub-hot-path-observability.md`, where queue pressure is logged locally and
handled by the hub event loop.

Backpressure is intentionally represented at two scopes. Hub-control
backpressure carries routing context, including the source, capacity,
`session_uuid`, and `client_id`, because the hub needs enough information to
decide policy and mutate orchestration state. Client-worker backpressure keeps
only the local source and capacity, because that mailbox is already scoped to a
single client and should not grow a parallel routing identity.

Outbound terminal delivery uses `try_send` into a bounded transport egress
queue. A full queue drops that hot-path frame and reports
`HubControlMessage::Backpressure` with the worker's `client_id` and the
session UUID for session-scoped traffic. Control and close frames are still
transport-neutral; close uses normal async send during shutdown so the adapter
has a chance to observe the reason.

Socket clients use the client worker's bounded outbound queue as the first
terminal-frame backpressure point. The socket writer queue remains a transport
adapter safety net; if it fills after worker egress accepted a frame, the
adapter requests the existing forced reconnect. In-process TUI delivery keeps
the existing unbounded `TuiOutput` renderer channel behind the adapter for
rendering compatibility, but the worker-to-adapter hop is bounded at 4096
messages so runaway renderer stalls still have a typed drop/backpressure point.
TUI input and hub-control paths flow through typed worker messages and report
bounded queue pressure where those mailboxes are bounded.

TUI bridge mechanics remain bridge-local. Length-prefixed socket framing,
hello/hello_ack protocol negotiation, quit interception, reconnect retries,
stale request draining, synthetic `bridge_reconnected` events, and wake-pipe
writes are adapter or bridge responsibilities, not client-worker policy.

Resize has an executable `SessionIoRequest` mailbox path. Terminal color
profile and other WebRTC JSON hub commands continue through the existing
Lua/hub JSON path unless they have an explicit typed worker message. Focus
changes do have a typed worker message:
`TransportIngress::FocusChanged` maps to `ClientControlFrame::FocusChanged`
before hub policy updates active terminal peer state. The terminal data plane
for TUI/local socket clients is workerized; generic control-plane JSON remains
hub-owned for this slice, but stable worker-owned control messages must not use
generic JSON fallbacks. JSON remains limited to Lua/plugin/relay boundaries and
the documented WebRTC subscribe acknowledgement bridge; new stable Botster-owned
controls need typed `TransportIngress`, `ClientWorkerMessage`, and
`TransportEgress` variants instead of `BoundaryJson`.

## Session Process Boundary

The session process remains minimal and per-session. It owns the PTY fd,
terminal parsing, snapshots, mode tracking, and process lifecycle. It does not
route clients, inspect browser state, or know about WebRTC. Worker contracts
must preserve that boundary: browser/TUI/socket concerns stop at the transport
adapter and client worker.
