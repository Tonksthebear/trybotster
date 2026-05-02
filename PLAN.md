# Plan: Worker Actor Contract Scaffolding

## Context And Constraints

Ticket: `ticket_1777747002_978782` - Introduce Worker Actor Contract Scaffolding.

Ticket description: implement the first repo-native scaffolding for the workerized architecture contracts: Hub control actor messages, generic `ClientWorker` messages, `TransportAdapter` boundary, `SessionIoWorker` messages, and durable Session process boundary docs/tests. This may start as typed Rust modules and architecture docs without routing all production traffic yet, but it should be executable/compiled where appropriate and verified. Ownership matters: hub mutates orchestration state, workers communicate through bounded queues, session process remains minimal and browser/WebRTC unaware.

Hard constraints from repo/vault:

- Hub is the central orchestrator and remains the state mutation owner.
- Workers must communicate through explicit bounded queues and typed messages; do not introduce another ambient shared-state surface.
- Session process remains minimal and client-transport agnostic. It owns PTY/session I/O and terminal parsing, not browser/WebRTC concepts.
- Browser and TUI are equal clients; `ClientWorker` contracts should be transport-neutral and adapters handle transport-specific concerns.
- CLI verification must use `cd cli && ./test.sh ...`, not raw `cargo test`.
- No Rails or UI work is required. `tmp/tailwind_plus_preview` is absent in this worktree; this ticket should not introduce Catalyst/Elements/Hotwire changes.

Relevant existing anchors:

- `cli/src/hub/events.rs` documents the current single hub event loop and unbounded `HubEvent` channel.
- `cli/src/hub/mod.rs` already has bounded WebRTC/send/input queues and hot-path queue capacity constants.
- `cli/src/hub/server_comms.rs` contains current transport handling and hot-path observability integration.
- `cli/src/session/protocol.rs`, `cli/src/session/connection.rs`, and `cli/src/session/mod.rs` define the existing per-session process wire boundary.
- `docs/hub-hot-path-observability.md` is the dependency-era doc for queue/hot-path diagnosis and should shape the worker contract docs.

## Implementation Sequence

1. Add a worker contract module tree under `cli/src/worker/`.
   - Add `cli/src/worker/mod.rs` and export it from `cli/src/lib.rs`.
   - Keep this as contract scaffolding: typed messages, handles, traits, constants, and docs. Avoid moving production traffic until a follow-up ticket explicitly does that.
   - Prefer small Rust enums/structs over stringly typed envelopes.

2. Define `HubControlActor` messages.
   - Create `cli/src/worker/hub_control.rs`.
   - Model only orchestration-facing commands/events that are stable enough now: client attach/detach intent, transport backpressure, session lifecycle notifications, and shutdown/reconnect coordination.
   - Include bounded queue config/constants and a handle shape that can be embedded later.
   - Make ownership explicit in docs: these messages request hub-owned mutations; workers do not mutate hub state directly.

3. Define the generic `ClientWorker` contract.
   - Create `cli/src/worker/client.rs`.
   - Define transport-neutral messages such as subscribe/unsubscribe session, outbound terminal bytes/control frame, connection health update, backpressure notice, and shutdown.
   - Keep browser/TUI-specific payloads out of the generic contract. Use opaque bytes or shared client/session identifiers where the transport adapter owns encoding.

4. Define the `TransportAdapter` boundary.
   - Create `cli/src/worker/transport.rs`.
   - Document and type the adapter responsibility: convert WebRTC/TUI/socket-specific ingress into `ClientWorker` messages and convert worker egress into transport-specific sends.
   - Do not import browser/WebRTC concepts into session process or session I/O contracts.

5. Define `SessionIoWorker` messages.
   - Create `cli/src/worker/session_io.rs`.
   - Mirror the durable session process boundary without replacing it: PTY input, resize, snapshot request/response, mode flags, plain screen request, color profile update, process exit, and structured terminal events.
   - Map to existing `cli/src/session/protocol.rs` frame concepts, but keep this as an internal Rust actor contract rather than a wire protocol rewrite.

6. Add durable architecture docs.
   - Add or update `docs/worker-actor-contracts.md`.
   - Cover ownership and data flow:
     - Hub control actor owns orchestration mutations.
     - Client workers are transport-neutral.
     - Transport adapters are the only browser/WebRTC/TUI/socket-specific layer.
     - Session I/O worker talks to the session process and emits session-scoped events.
     - Session process stays minimal, per-session, and browser/WebRTC unaware.
   - Include bounded queue/backpressure expectations and how this builds on hot-path observability.

7. Add focused compile-time and unit coverage.
   - Add tests in the new worker modules or a focused `cli/tests/worker_actor_contract_test.rs`.
   - Verify message types are constructible, clone/debug where needed, bounded queue configs are nonzero, and transport/session contracts do not require concrete WebRTC/browser types.
   - Add a doc/reference test only if it helps pin a contract; avoid brittle architecture prose tests.

## Non-Goals

- Do not route all production hub traffic through these actors in this ticket.
- Do not change the existing session process wire protocol unless a compile-time mapping requires a small additive helper.
- Do not add Project Pipelines plugin persistence or UI around worker actors.
- Do not add Rails, React, Catalyst, Elements, Hotwire, or Tailwind changes.

## Verification Plan

Primary commands:

```bash
cd cli && ./test.sh --unit -- worker
cd cli && ./test.sh --unit -- session
```

If the test filter cannot select the new unit coverage cleanly, run:

```bash
cd cli && ./test.sh --unit
```

Static review checks:

- New modules compile through `cli/src/lib.rs`.
- No new unbounded channel is introduced for worker data planes.
- Session process contract docs remain browser/WebRTC agnostic.
- Hub mutation ownership is explicit in module docs and architecture docs.

## Gate Evidence

Plan artifact: `PLAN.md`.

Repo/vault evidence used:

- `AGENTS.md` and `CLAUDE.md` for session and CLI test conventions.
- `project_pipelines_current_context` for the actual ticket description and gate prompt.
- `docs/hub-hot-path-observability.md` for the prior dependency’s hot-path and bounded queue framing.
- `cli/src/hub/events.rs`, `cli/src/hub/mod.rs`, `cli/src/hub/server_comms.rs`, `cli/src/session/protocol.rs`, `cli/src/session/connection.rs`, and `cli/src/session/mod.rs` for current ownership and process boundaries.
