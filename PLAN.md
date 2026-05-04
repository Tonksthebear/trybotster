# Plan: Align Workerized Architecture Docs And Static Checks

Ticket: `ticket_1777787693_701304` - Align workerized architecture docs and
static checks with production reality.

## Intent

This ticket is a post-migration alignment pass. Several workerization slices
have landed, but the repo still needs the architecture documents and regression
checks to describe the implementation that actually exists, not a stronger
aspirational target.

The work is to audit `docs/worker-actor-contracts.md`, any related verification
notes, and the production Rust paths for WebRTC, client workers, and session I/O
mailboxes. Then update the docs so they state the canonical runtime boundary
precisely:

- `WebRtcPeerRegistry` owns browser WebRTC peer/channel state and production
  data-plane receiver forwarders.
- `WebRtcTransportRunner` and `WebRtcTransportAdapter` classify browser
  DataChannel JSON into typed `TransportIngress` / `ClientWorkerMessage` where
  stable Botster-owned controls exist.
- The hub still owns orchestration policy, Lua routing, auth/capability checks,
  Rails relay coordination, and handshake-time encrypted answer generation.
- Browser terminal subscribe/input/focus traffic must enter the browser
  `ClientWorker`; stable browser terminal traffic must not bypass the worker
  through an adapter-only or direct hub path.
- Session paste and snapshot preparation are worker mailbox work only after hub
  authorization/routing policy has selected the session and destination.

This is an architecture cleanup, so the old behavior to remove or make
impossible is ambiguity: docs that overclaim full worker ownership where the hub
still intentionally owns policy or handshake exceptions, docs that describe
scaffold-only request variants as production paths, and brittle text checks that
only match old field names while allowing equivalent boundary violations under
new names.

## Hard Constraints

- Use `cd cli && ./test.sh ...` for CLI verification, not raw `cargo test`.
- Preserve the hub/client-worker/session-I/O ownership split from the vault:
  hub is the central orchestrator; workers own transport-neutral stream or
  session data-plane mechanics and request hub policy through typed messages.
- Keep the cold-turkey migration posture for architectural ambiguity: remove or
  rewrite misleading claims rather than preserving old and new wording side by
  side.
- Keep boundary exceptions explicit instead of hiding them. In particular,
  handshake-time SDP answer encryption remains hub-triggered policy work today,
  while the registry/runner owns the transport mechanics around it.
- Do not introduce new gems, build steps, Node-only checks, or bespoke
  framework abstractions. Static checks should live in Rust tests or simple repo
  scripts already covered by `cli/test.sh`.
- No UI work is planned. `tmp/tailwind_plus_preview` is absent in this
  worktree, and no browser component changes are needed. If UI work unexpectedly
  appears during implementation, use the existing React/Vite Catalyst components
  and local `ui_contract` primitives instead of inventing controls or styling.

## Affected Surfaces

- `docs/worker-actor-contracts.md`
  - Main documentation target.
  - Reconcile any overly strong production claims about
    `WebRtcTransportRunner`, `ClientWorker`, paste/snapshot ownership, and raw
    JSON escape hatches with current code.
  - Keep intentional exceptions called out with mechanism and scope.

- `docs/project-pipelines-verification.md`
  - Add a dated verification entry with the exact focused/full checks and static
    scan results after implementation.

- `cli/src/worker/webrtc.rs`
  - Production source for registry-owned receivers, `WebRtcTransportRunner`,
    WebRTC ingress classification, and allowed `BoundaryJson` cases.
  - Static tests should assert the shape of these boundaries without depending
    on private field names alone.

- `cli/src/worker/transport.rs`
  - Production source for generic typed ingress/egress mapping and allowed
    `BoundaryJson` conversions for Lua/plugin/relay boundaries.

- `cli/src/worker/session_io.rs` and
  `cli/src/worker/session_io_runtime.rs`
  - Production source for mailbox variants and which variants are actually
    executable worker work.
  - Static checks should prevent scaffold-only variants from being documented as
    production-owned if they still only return scaffold errors.

- `cli/src/hub/server_comms.rs`
  - Production source for hub-owned policy, browser worker registration,
    WebRTC event handling, paste authorization, snapshot routing, and
    handshake-time answer encryption.
  - Static checks should distinguish allowed hub policy/relay exceptions from
    forbidden transport receiver ownership or browser terminal bypasses.

- `cli/src/worker/mod.rs` or a focused `cli/tests/...` integration-style test
  if the check cannot live cleanly beside module tests.
  - Preferred home for semantic static checks that read source files with
    `include_str!` and assert boundary invariants.

## Implementation Sequence

1. Audit current production behavior.
   - Trace browser DataChannel ingress from
     `WebRtcPeerRegistry::handle_plaintext_payload` through
     `WebRtcTransportRunner`, `WebRtcTransportAdapter`,
     `TransportIngress`, and `ClientWorkerMessage`.
   - Trace WebRTC production receiver ownership through
     `WebRtcPeerRegistry::start_queue_forwarders`.
   - Trace hub-side exceptions in `server_comms.rs`: policy handling,
     `process_webrtc_plaintext_payload`, Lua fallback routing, paste
     authorization, snapshot routing, and `WebRtcTransportRunner::negotiate_offer`.
   - Trace `SessionIoRequest` variants to separate production mailbox work
     (`PtyInput`, `PasteFile`, `PrepareSnapshot`, shutdown/resize/color profile
     if wired) from scaffold-only synchronous RPC mirrors if they still return
     scaffold errors.

2. Rewrite docs to match actual runtime ownership.
   - Update `docs/worker-actor-contracts.md` so every strong claim has a
     matching production mechanism.
   - Replace aspirational wording such as "all WebRTC production traffic flows
     through ClientWorker" with the narrower true statement: browser terminal
     subscribe/input/focus and typed terminal controls cross the client-worker
     boundary, while Lua/plugin/relay JSON remains a documented boundary
     exception.
   - Keep the expected target path visible, but label any remaining bridge as
     current production behavior rather than future scaffolding.
   - Document handshake-time answer encryption as an intentional hub-triggered
     exception if it remains implemented via
     `WebRtcTransportRunner::negotiate_offer` spawned from hub policy.

3. Replace brittle static checks with semantic boundary tests.
   - Add focused static tests that parse/read source text structurally enough to
     assert relationships, not only deleted field names.
   - WebRTC receiver ownership check:
     assert production hub code does not call `lease_*_receiver_for_test`,
     `poll_received_messages`, or raw WebRTC receiver `take()` paths outside
     `#[cfg(test)]` test-driving helpers, and assert
     `start_queue_forwarders` emits typed `HubEvent` variants.
   - Browser client-worker ingress check:
     assert browser subscribe/unsubscribe/focus classifications become
     `ClientWorkerMessage` variants and `process_webrtc_plaintext_payload`
     forwards `WebRtcIngressOutcome::ClientWorker` into
     `browser_client_workers`.
   - Adapter-only bypass check:
     assert browser terminal PTY input/file input/snapshot delivery does not
     directly encode/send terminal traffic from `server_comms.rs` without going
     through `ClientWorkerMessage` or a documented Lua/relay boundary.
   - Session-I/O mailbox check:
     assert docs only claim production mailbox ownership for variants with real
     runtime handling, and specifically catch scaffold-only
     `SessionIoRequest` variants being documented as production-owned while
     `session_io_runtime.rs` still returns scaffold errors.
   - JSON escape-hatch check:
     assert `BoundaryJson` usage is confined to transport adapter conversion
     and Lua/plugin/relay boundary handling, not stable Botster-owned control
     frames that already have typed variants.

4. Remove or tighten misleading checks.
   - Find any existing `rg`-style architecture checks in docs, tests, or
     verification notes that only look for old fields such as removed receiver
     names.
   - Replace them with the new focused tests or update the verification command
     list so future agents run the semantic checks rather than stale scans.

5. Verify with focused CLI checks.
   - Run the smallest relevant filters first:

     ```bash
     cd cli && ./test.sh --unit -- worker
     cd cli && ./test.sh --unit -- server_comms
     ```

   - Run any new focused static-check filter by name.
   - Run full CLI unit verification before handoff:

     ```bash
     cd cli && ./test.sh --unit
     ```

6. Record evidence.
   - Add a dated entry to `docs/project-pipelines-verification.md` with command
     output summaries.
   - Include the static check names and the exceptions they intentionally allow.
   - Note that no UI changed and that Catalyst/Elements/Tailwind preview review
     was not applicable.

## Acceptance Checklist

- `docs/worker-actor-contracts.md` describes the workerized architecture as it
  exists in production, including hub-owned policy and handshake exceptions.
- Any scaffold-only session-I/O request variants are either documented as
  scaffold/currently-not-production or removed from production-ownership claims.
- Browser terminal traffic cannot regress to bypassing the browser
  `ClientWorker` without a failing static or behavioral test.
- Hub code cannot reintroduce production raw WebRTC receiver leasing/polling
  without a failing static test; test-only helpers remain clearly `#[cfg(test)]`.
- Raw JSON escape hatches are limited to documented Lua/plugin/relay boundaries;
  stable Botster-owned controls remain typed.
- Verification uses `cli/test.sh`, and results are recorded in
  `docs/project-pipelines-verification.md`.
