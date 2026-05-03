# Plan: Tighten Typed Worker Control Frames

Ticket: `ticket_1777787732_595161` - Tighten typed worker control frames and reduce JSON escape hatches.

## Intent

The worker actor contract should make it impossible to express a stable
Botster-owned protocol message as an arbitrary `serde_json::Value`.

`ClientControlFrame::Json` and `TransportEgress::Control` currently let Hub and
worker code send known messages such as `terminal_attach`, kitty/focus mode
changes, `pong`/`dc_pong`, and some lifecycle frames through broad JSON
catchalls. That was useful scaffolding, but it weakens the typed worker boundary:
callers can invent malformed stable frames and adapters cannot prove conversion
losslessness.

Replace stable Botster-owned shapes with Rust enum variants. Keep JSON only
where the shape is defined outside the Rust worker contract: Lua/plugin payloads,
browser/WebRTC JSON payloads that must be handed to Lua routing, and opaque
ActionCable/WebRTC relay envelopes.

No UI work is planned. If implementation unexpectedly touches UI, inspect
`tmp/tailwind_plus_preview` and use the existing React/Catalyst/local primitives;
do not invent controls or styling.

## Hard Constraints

- Hub remains the central orchestrator. Workers own transport-neutral stream
  state and ask Hub for policy through typed messages.
- TUI, socket, and browser terminal streams continue to use the
  `ClientWorker`/transport-adapter boundary.
- Cold-turkey migration: when a stable JSON shape gets a typed variant, remove
  the old `ClientControlFrame::Json` construction in the same change. Do not
  keep fallback JSON paths for migrated stable messages.
- `serde_json::Value` may remain only at named Lua/plugin/relay boundaries.
- CLI tests must use `cd cli && ./test.sh ...`, never raw `cargo test`.

## JSON Boundary Policy

A frame stays JSON iff its schema is owned outside the Rust worker contract:

- Lua/plugin boundary: `TuiRequest::LuaMessage`, `TuiOutput::Message`, plugin
  binary/message payloads, and Lua callback payloads whose shape is intentionally
  plugin-defined.
- Browser/WebRTC routing boundary: decrypted browser messages whose `type` is
  routed to Lua/plugin handlers or signaling/control paths not owned by the
  terminal worker contract.
- Relay boundary: ActionCable/Olm/signaling envelopes, ICE candidates, encrypted
  offers/answers, and Rails-relayed payloads.

Everything else is Botster-owned and must be typed.

## Typed Variants To Add

In `cli/src/worker/client.rs`, extend `ClientControlFrame`:

- `Pong { request_id: RequestId }`
- `TerminalAttach { subscription_id: SubscriptionId, session_uuid: SessionUuid, state: TerminalAttachState }`
- `KittyChanged { session_uuid: SessionUuid, enabled: bool }`
- `FocusReportingChanged { session_uuid: SessionUuid, enabled: bool }`
- Keep existing typed `Snapshot`, `Scrollback`, `ModeChanged`, and
  `ProcessExited`.
- Rename the generic `Json(serde_json::Value)` to an explicitly scoped boundary
  variant such as `PluginJson(serde_json::Value)` only if remaining callers are
  true Lua/plugin boundary payloads. If no boundary caller needs it inside
  `ClientControlFrame`, remove it entirely.

In `cli/src/worker/transport.rs`, extend `TransportIngress`/`TransportEgress`:

- `TransportIngress::FocusChanged { session_uuid, focused }`
- `TransportIngress::DcPing`
- `TransportIngress::DcPong`
- `TransportIngress::BoundaryJson(serde_json::Value)` for true Lua/plugin/relay
  ingress only.
- `TransportEgress::Pong { request_id }`
- `TransportEgress::TerminalAttach { subscription_id, session_uuid, state }`
- `TransportEgress::KittyChanged { session_uuid, enabled }`
- `TransportEgress::FocusReportingChanged { session_uuid, enabled }`
- `TransportEgress::DcPong`
- Rename `Control(serde_json::Value)` to `BoundaryJson(serde_json::Value)` only
  if a true boundary egress remains.

Use a narrow `TerminalAttachState` enum for known states currently sent as
strings, at least `Attached`, `Reconnecting`, and `NotFound` if those are the
only values used. If existing code has additional terminal attach states, add
them explicitly during implementation rather than preserving arbitrary strings.

## Call-Site Disposition

Current `rg -n 'ClientControlFrame::Json|TransportEgress::Control|TransportIngress::Json' cli/src`
sites and planned disposition:

- `cli/src/worker/client.rs:514`
  - Replace `ClientControlFrame::Json(value)` handling with typed arms for
    `Pong`, `TerminalAttach`, `KittyChanged`, and `FocusReportingChanged`.
  - Any remaining JSON arm must be renamed to boundary-only and documented.
- `cli/src/worker/client.rs:538`
  - Replace `deliver_control(... TransportEgress::Control(value) ...)` with
    typed egress constructors. Delete this helper if it only exists for stable
    JSON controls.
- `cli/src/worker/client.rs:865`
  - Update ping test to expect `TransportEgress::Pong { request_id: "req-1" }`,
    not JSON `{ "type": "pong" }`.
- `cli/src/worker/transport.rs:151`
  - Socket `Frame::Json` unknown values become `TransportIngress::BoundaryJson`
    only for plugin/Lua messages; stable JSON frame types are decoded to typed
    ingress variants where known.
- `cli/src/worker/transport.rs:192`
  - Replace `TransportEgress::Control(value) => Frame::Json(value)` with
    explicit encoding for `Pong`, `TerminalAttach`, `KittyChanged`,
    `FocusReportingChanged`, and boundary JSON.
- `cli/src/worker/transport.rs:232`
  - Keep `TuiRequest::LuaMessage(value)` as boundary JSON because it is the Lua
    client protocol.
- `cli/src/worker/transport.rs:239`
  - Replace `TuiRequest::FocusChanged` JSON construction with
    `TransportIngress::FocusChanged { session_uuid, focused }`.
- `cli/src/worker/transport.rs:276`
  - Replace `TransportEgress::Control(value) => TuiOutput::Message(value)` with
    explicit encoding for typed controls plus boundary JSON.
- `cli/src/worker/transport.rs:316-317`
  - Replace generic ingress JSON to `ClientControlFrame::Json` with typed
    mapping for stable controls and boundary-only JSON for Lua/plugin payloads.
- `cli/src/worker/transport.rs:339-340`
  - Replace `ClientControlFrame::Json` to `TransportEgress::Control` with typed
    egress arms.
- `cli/src/worker/mod.rs:191-192`
  - Test echo adapter should either use boundary JSON explicitly or avoid JSON;
    do not model stable control frames with the catchall.
- `cli/src/worker/webrtc.rs:1103-1104`
  - Decode WebRTC stable heartbeat/control ingress (`dc_ping`, `dc_pong`) to
    typed ingress. Route other browser JSON as boundary JSON only when it is
    handed to Lua/plugin/WebRTC routing.
- `cli/src/worker/webrtc.rs:1122-1123`
  - Replace `ClientControlFrame::Json` to `TransportEgress::Control` with typed
    egress and boundary-only JSON.
- `cli/src/hub/server_comms.rs:3283-3287`
  - Stop round-tripping inbound WebRTC JSON through `ClientControlFrame::Json`
    just to recover the same value. Classify `dc_ping`/`dc_pong` before Lua
    routing, using typed ingress for those stable heartbeat frames. Keep other
    browser messages as boundary JSON for `call_lua_webrtc_message`.
- `cli/src/hub/server_comms.rs:3305`
  - Replace direct `serde_json::json!({ "type": "dc_pong" })` construction with
    a typed adapter command or `TransportEgress::DcPong` encoder while
    preserving immediate heartbeat response behavior.
- `cli/src/hub/server_comms.rs:3918`
  - Replace `ClientControlFrame::Json(payload)` in
    `send_worker_terminal_attach_state` with
    `ClientControlFrame::TerminalAttach`.
- `cli/src/hub/server_comms.rs:4846`
  - Replace initial TUI `terminal_attach` JSON with
    `ClientControlFrame::TerminalAttach`.
- `cli/src/hub/server_comms.rs:4959`
  - Replace stashed `kitty_changed` JSON with
    `ClientControlFrame::KittyChanged`.
- `cli/src/hub/server_comms.rs:4970`
  - Replace stashed `focus_reporting_changed` JSON with
    `ClientControlFrame::FocusReportingChanged`.
- `cli/src/hub/server_comms.rs:5004`
  - Replace live `kitty_changed` JSON with `ClientControlFrame::KittyChanged`.
- `cli/src/hub/server_comms.rs:5015`
  - Replace live `focus_reporting_changed` JSON with
    `ClientControlFrame::FocusReportingChanged`.
- `cli/src/hub/server_comms.rs:5247`
  - Replace socket initial `terminal_attach` JSON with
    `ClientControlFrame::TerminalAttach`.
- `cli/src/hub/server_comms.rs:5341`
  - Replace socket `kitty_changed` JSON with
    `ClientControlFrame::KittyChanged`.
- `cli/src/hub/server_comms.rs:5352`
  - Replace socket `focus_reporting_changed` JSON with
    `ClientControlFrame::FocusReportingChanged`.

Existing typed lifecycle frames:

- `ClientControlFrame::Snapshot`, `Scrollback`, `ModeChanged`, and
  `ProcessExited` already exist. Preserve them and ensure adapter tests cover
  their conversions where they are part of the stable worker contract.

## File-Level Implementation Sequence

1. Extend worker contract types.
   - Edit `cli/src/worker/client.rs` for new `ClientControlFrame` variants and
     `TerminalAttachState`.
   - Edit `cli/src/worker/transport.rs` for typed ingress/egress variants.

2. Update ClientWorker dispatch.
   - Convert `Ping` handling to emit `TransportEgress::Pong`.
   - Replace `deliver_control` JSON helper with typed egress routing.
   - Delete stable-shape `ClientControlFrame::Json` handling.

3. Update transport adapters.
   - Socket adapter: decode known JSON control frame types into typed ingress
     and encode typed egress back to the current socket wire JSON or binary
     frame shapes.
   - TUI adapter: map `TuiRequest::FocusChanged` to typed ingress and encode
     typed egress to the existing `TuiOutput` wire shape.
   - WebRTC adapter: decode `dc_ping`/`dc_pong` as typed heartbeat ingress and
     encode `DcPong` through the current WebRTC JSON wire shape.
   - Keep boundary JSON explicitly named for Lua/plugin/relay messages.

4. Update Hub call sites.
   - Replace the `server_comms.rs` terminal attach, kitty, focus, and heartbeat
     JSON constructions listed above.
   - Keep Lua/plugin routing values as JSON only where they match the boundary
     policy.

5. Delete dead escape hatches.
   - Remove `ClientControlFrame::Json` entirely if possible.
   - If boundary JSON must remain, rename it so any future stable-control use is
     visually wrong in review.
   - Remove or rename `TransportEgress::Control`.

6. Add lossless conversion tests.
   - Add inline `#[cfg(test)]` coverage beside adapter conversions in
     `cli/src/worker/transport.rs` and `cli/src/worker/webrtc.rs`, or a small
     `cli/src/worker/transport_conversion_tests.rs` module if the table grows.
   - Cover each typed variant:
     - `Pong`
     - `TerminalAttach`
     - `KittyChanged`
     - `FocusReportingChanged`
     - `DcPong`
     - Existing `Scrollback`, `ModeChanged`, and `ProcessExited`
   - For adapters with JSON wire output, assert field-for-field preservation:
     typed egress -> wire JSON/frame -> ingress/client message where applicable.
   - For boundary JSON, assert it remains pass-through only under the renamed
     boundary variant and does not claim to be a stable control frame.

7. Update tests that currently assert JSON catchalls.
   - `cli/src/worker/client.rs` ping test expects typed `Pong`.
   - `cli/src/worker/transport.rs` adapter tests expect typed conversions.
   - `cli/src/worker/webrtc.rs` tests cover typed heartbeat classification.
   - `cli/src/hub/server_comms.rs` terminal attach tests keep asserting external
     behavior but should no longer require `ClientControlFrame::Json`.

## Verification Commands

Run from the repo root:

```bash
cd cli && ./test.sh --unit worker
cd cli && ./test.sh --unit server_comms
cd cli && ./test.sh --unit
```

Static checks after implementation:

```bash
rg -n 'ClientControlFrame::Json|TransportEgress::Control' cli/src/worker cli/src/hub cli/src/socket
rg -n 'serde_json::json!\(\{\s*"type": "(terminal_attach|kitty_changed|focus_reporting_changed|pong|dc_pong|focus_changed)"' cli/src/worker cli/src/hub cli/src/socket
```

Expected static-check result:

- No `ClientControlFrame::Json` construction for stable Botster-owned controls.
- No `TransportEgress::Control` for stable Botster-owned controls.
- Any remaining JSON matches must be renamed boundary variants or adapter-local
  wire encoders, with an explicit Lua/plugin/relay justification.
