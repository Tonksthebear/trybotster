# Worker Actor Contracts

Botster's workerized runtime boundary starts with typed Rust contracts. Some
contracts are scaffolding for later slices; the session I/O worker is now the
production read path for durable session-process PTY output.

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
still owns writes and synchronous RPC methods, while the worker decodes every
socket frame and routes snapshot, screen, mode-flags, and other control
responses back to the existing `response_rx` channel. This avoids socket read
contention without moving hub orchestration policy into the worker.

File paste/drop data-plane mechanics live in `worker::session_io`. The hub
still authorizes the target session and transport capabilities, then passes the
already-authorized filename and bytes to the session I/O helper. Paste temp
paths are session-scoped and resolve in this order: session manifest
`worktree_path` under `.botster/pastes/<session_uuid>`, Botster data dir under
`pastes/<session_uuid>`, then the OS temp dir under
`botster/pastes/<session_uuid>`. Cleanup is keyed by session UUID, never label,
and runs on real process exit plus hub drop.

Snapshot payload preparation also lives in the session I/O data-plane contract.
The hub remains responsible for deciding when a snapshot is needed and where it
should be routed. `request_id` is an opaque correlation key scoped to the
session; callers use it to map prepared output back to peer/subscription or Lua
refresh state. Browser identities and WebRTC structs do not enter the worker
contract.

The paste and prepared-snapshot request/event variants define the mailbox
contract for the next production-wiring slice. Today the production hub calls
the `worker::session_io` helpers directly, so the data-plane byte/path logic is
already centralized while mailbox send/receive ownership remains scaffolding.

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
transport egress queue. Production hub traffic still needs explicit wiring onto
this actor; the current implementation proves the runtime contract without
moving WebRTC, TUI, or socket send loops into the worker.

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

## Session Process Boundary

The session process remains minimal and per-session. It owns the PTY fd,
terminal parsing, snapshots, mode tracking, and process lifecycle. It does not
route clients, inspect browser state, or know about WebRTC. Worker contracts
must preserve that boundary: browser/TUI/socket concerns stop at the transport
adapter and client worker.
