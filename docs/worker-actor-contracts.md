# Worker Actor Contracts

Botster's workerized runtime boundary starts with typed Rust contracts. Some
contracts are scaffolding for later slices; the session I/O worker is now the
production read path for durable session-process PTY output, and WebRTC peer
state now lives behind the client transport adapter boundary.

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

Session I/O workers mirror the current per-session process protocol. They carry
PTY input, resize, snapshot, mode flags, plain screen, color profile, authorized
file paste/drop payloads, prepared snapshot payloads, structured terminal
events, and process-exit messages as Rust actor messages. The Unix socket wire
protocol in `cli/src/session/protocol.rs` remains the durable process boundary.

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

PTY output crosses into the hub as `HubEvent::SessionIoBatch`, not one hub
event per `FRAME_PTY_OUTPUT`. The worker preserves byte order while coalescing
output until any of these flush boundaries is reached:

- 32 KiB of buffered output
- 16 output frames
- 4 ms since the first buffered output or metadata update
- an ordered structured event such as prompt mark, bell, notification, or
  process exit
- EOF, protocol desync, or worker shutdown

Sparse terminal metadata is coalesced in the same short window: mode fields
merge with last value winning, title and CWD keep the last value, and prompt
marks, bells, notifications, and process exits remain ordered boundaries.

Browser and TUI clients still receive the existing `PtyEvent` payloads. Lua
`pty_output` observers intentionally see the coalesced byte chunks emitted by
`SessionIoBatch` rather than the session protocol's original frame boundaries;
total bytes and byte order are preserved.

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

`worker::client::ClientWorker::start` is the executable core for this boundary.
It creates a bounded client mailbox, records subscriptions by session UUID,
emits hub-owned attach/detach/reconnect/shutdown/backpressure requests, routes
subscribed `SessionInput` frames to attached `SessionIoRequest::PtyInput`
mailboxes, and forwards subscribed session output/control frames to the
transport egress queue. TUI and local socket terminal streams are now
production-wired through this actor: hub-owned Lua attach requests still decide
when a session exists, when pending attach intents resolve, and when forwarder
tasks are cleaned up, but snapshot, live PTY output, process-exit, JSON control,
plugin binary, and raw input traffic cross the client-worker boundary before a
TUI or socket adapter encodes them. WebRTC production traffic enters the
transport adapter boundary through `worker::webrtc::WebRtcPeerRegistry` and
`WebRtcTransportRunner`; the adapter converts decoded ingress into
`ClientWorkerMessage` before hub policy handles the legacy Lua routing surface.

## WebRTC Transport Adapter

`WebRtcPeerRegistry` is the only holder of per-browser `WebRtcChannel`
instances. The Hub owns one registry handle and keeps auth, pairing,
capability, Rails relay coordination, Lua callbacks, terminal attach policy,
and summarized cleanup policy. WebRTC peer connection state, offer generation,
pending ICE queues, per-peer bounded send queues, DataChannel liveness pings,
unknown-peer burst coalescing, backpressure recovery tracking, and peer cleanup
bookkeeping stay inside the registry.

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
through `WebRtcPeerRegistry::queue_command` or the narrower PTY helpers. The
registry owns the bounded per-peer command channel and the async send task that
serializes PTY, JSON, stream, binary, and bundle-refresh frames onto the
DataChannel. Binary adapter commands must use the raw DataChannel send helper;
JSON helpers are only for JSON frames.

WebRTC transport summaries cross back to the Hub through typed
`HubControlMessage` variants:

- `TransportPeerStateChanged`
- `TransportSignalReady`
- `TransportBackpressure`
- `TransportRatchetRestartRequested`

`TransportSignal` envelopes intentionally carry `serde_json::Value` only at
the Rails relay boundary because those values are already serialized Olm
envelopes. Do not let that exception spread to new adapter control surfaces.

Crypto ownership is split by lifecycle phase. DataChannel encrypt/decrypt
failure tracking and ratchet-delivery transport are adapter/registry concerns.
The Hub still generates fresh ratchet bundles and, for now, performs
handshake-time SDP answer encryption before emitting `TransportSignalReady`;
that is a known follow-up boundary, not permission to move hot-path
DataChannel crypto back into Hub code.

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

The first client-worker runtime accepts the session I/O sender map at start
time. Production hub wiring that creates or tears down session I/O workers while
a client worker stays alive should add explicit attach/detach messages for
those senders before relying on long-lived client workers for dynamic session
sets.

Reconnect generation is tracked by the client worker. Frames wrapped with an
older generation are dropped before delivery, and reconnect health emits a
typed `HubControlMessage::Reconnect` so hub policy stays centralized. `Ping`
has an explicit observability response: the worker emits a transport-neutral
`TransportEgress::Control` pong with the original request ID.

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
client stream is over. If a client subscribes to the same session UUID with a
new subscription ID, the worker emits a fresh `AttachClient`; hub routing policy
must treat that as replacement or the worker should grow an explicit
old-subscription detach before production traffic depends on this path.

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

Resize, focus, terminal color profile, and other JSON hub commands continue
through the existing Lua/hub JSON path unless they have an explicit typed worker
message. The terminal data plane for TUI/local socket clients is workerized;
generic control-plane JSON remains hub-owned for this slice.

## Session Process Boundary

The session process remains minimal and per-session. It owns the PTY fd,
terminal parsing, snapshots, mode tracking, and process lifecycle. It does not
route clients, inspect browser state, or know about WebRTC. Worker contracts
must preserve that boundary: browser/TUI/socket concerns stop at the transport
adapter and client worker.
