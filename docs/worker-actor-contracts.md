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
subscriptions and outbound terminal/control messages using
`worker::client::ClientWorkerMessage`. They identify clients with the existing
`ClientId` type so browser, TUI, socket, and internal clients stay equal.

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

## Session Process Boundary

The session process remains minimal and per-session. It owns the PTY fd,
terminal parsing, snapshots, mode tracking, and process lifecycle. It does not
route clients, inspect browser state, or know about WebRTC. Worker contracts
must preserve that boundary: browser/TUI/socket concerns stop at the transport
adapter and client worker.
