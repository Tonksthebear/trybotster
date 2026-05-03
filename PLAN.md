# Plan: Introduce Generic Client Worker Core

## Context And Constraints

Ticket: `ticket_1777747011_119144` - Introduce Generic Client Worker Core.

Implement the generic `ClientWorker` actor core that owns per-client identity,
session subscriptions, attached session streams, outbound queue policy,
liveness state, and routing between hub control frames and session stream
frames. It must remain transport-agnostic and communicate with the hub and
`SessionIoWorker` boundary through bounded queues.

Hard constraints from repo/vault:

- The hub remains the central orchestrator and sole owner of orchestration
  state. Client workers may request attach/detach/reconnect/shutdown policy,
  but must not mutate hub registries directly.
- Browser and TUI are equal clients through `ClientId`; no browser/WebRTC or
  TUI-specific logic belongs inside the core worker.
- Session processes stay minimal and browser/WebRTC unaware; session I/O
  remains session-scoped and communicates through typed worker messages.
- Use the worker contract scaffolding from the closed dependency ticket:
  `cli/src/worker/client.rs`, `hub_control.rs`, `session_io.rs`, and
  `transport.rs`.
- Prefer typed Rust enums/structs over stringly envelopes. The existing
  `serde_json::Value` escape hatches are tolerable only where current hub
  control payloads have not yet been typed.
- All new hot-path queues must be bounded and report/drop deterministically for
  slow-client isolation.
- CLI verification must use `cd cli && ./test.sh ...`, never raw `cargo test`.
- No UI work is planned. `tmp/tailwind_plus_preview` is absent in this
  worktree; if UI scope appears later, inspect the repo design system first and
  use existing Catalyst/Elements/Hotwire/local primitives instead of inventing
  styling or controls.

## Implementation Scope

### 1. Build The Client Worker Core

Extend `cli/src/worker/client.rs` with a concrete transport-neutral
`ClientWorker` runtime:

- State:
  - `client_id: ClientId`
  - subscriptions keyed by `SessionUuid`, preserving each transport-local
    `SubscriptionId`
  - attached session stream senders keyed by `SessionUuid`
  - connection health/liveness state and reconnect generation
  - outbound queue policy and counters for sent/dropped/backpressure events
- Constructor/start API:
  - create a bounded mailbox using `CLIENT_WORKER_QUEUE`
  - accept bounded senders for hub-control messages and session-I/O requests
  - accept a bounded outbound transport sender or generic egress sink
  - return `ClientWorkerHandle` so future hub code can own the mailbox without
    knowing worker internals
- Message loop:
  - `SubscribeSession` records the subscription and emits
    `HubControlMessage::AttachClient`
  - `UnsubscribeSession` removes the subscription and emits
    `HubControlMessage::DetachClient`
  - `TerminalBytes` forwards only to subscribed sessions
  - `ControlFrame` routes session-scoped frames only to attached/subscribed
    sessions; global JSON control remains client-scoped
  - `Health` updates liveness, handles reconnect generations, and suppresses
    delivery while disconnected/reconnecting
  - `Backpressure` emits typed `HubControlMessage::Backpressure`
  - `Shutdown` closes outbound delivery and emits hub shutdown/reconnect policy
    as appropriate

### 2. Make Slow-Client Isolation Explicit

Add an outbound policy type near the worker core:

- Use bounded `tokio::sync::mpsc` senders and `try_send` on hot-path terminal
  frames.
- For terminal-byte overload, drop or coalesce according to one explicit policy
  and emit backpressure to hub control with `client_id`, `session_uuid`, source,
  and capacity.
- For control frames and shutdown/close frames, prefer reliable delivery or
  forced reconnect over silent loss.
- Ensure one slow client cannot block other clients, hub event handling, or
  session I/O worker delivery.

### 3. Add A Minimal Test Adapter

Add a test-only adapter/sink proving the transport seam:

- Keep it in `cli/src/worker/client.rs` tests or a small sibling test module,
  unless production integration needs a reusable type.
- Convert worker output into transport-neutral `TransportEgress` without
  importing WebRTC, socket, or TUI concrete transport modules.
- Exercise one TUI-like and one browser-like `ClientId` through the same worker
  path to prove equality at the core layer.

### 4. Keep Production Wiring Minimal

This ticket should not reroute the whole hub data plane unless required by the
implementation. Acceptable production-facing changes:

- Export the worker start/handle types from `cli/src/worker/mod.rs`.
- Add helper constructors or typed adapters needed for compilation and tests.
- Update `docs/worker-actor-contracts.md` to describe the new concrete
  `ClientWorker` behavior and where production traffic is still pending.

Avoid moving WebRTC/TUI/socket send loops into the worker in this ticket unless
the change stays small and testable. The goal is a real, executable core plus
adapter proof, not a flag-day hub rewrite.

## Verification Plan

Focused tests:

```bash
cd cli && ./test.sh --unit worker
```

If the test wrapper cannot target module names reliably, use the nearest
focused equivalent supported by `cli/test.sh`, then record the exact command.

Expected coverage:

- bounded client-worker mailbox and outbound queue behavior
- subscribe/unsubscribe attach/detach hub-control messages
- terminal bytes sent only to subscribed sessions
- process/snapshot/control frames respect subscription state
- disconnected/reconnecting health suppresses or closes outbound delivery
- slow outbound sink emits typed backpressure without blocking
- same core path works for browser and TUI `ClientId`
- adapter proof contains no concrete WebRTC/browser/TUI transport imports

Static checks:

```bash
rg -n "WebRtc|Browser\\(|TuiOutput|SocketClientConn" cli/src/worker/client.rs
rg -n "ClientWorker|HubControlMessage|SessionIoRequest|TransportEgress" cli/src/worker docs/worker-actor-contracts.md
```

Broader checks if implementation touches hub event handling:

```bash
cd cli && ./test.sh --unit hub
```
