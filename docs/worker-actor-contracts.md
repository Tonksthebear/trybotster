# Worker Actor Contracts

Botster's workerized runtime boundary starts with typed Rust contracts. This
document describes those contracts without claiming production traffic has moved
onto them yet.

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
PTY input, resize, snapshot, mode flags, plain screen, color profile, structured
terminal events, and process-exit messages as Rust actor messages. The Unix
socket wire protocol in `cli/src/session/protocol.rs` remains the durable
process boundary.

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
