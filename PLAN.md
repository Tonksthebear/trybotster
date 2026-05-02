# Plan: Generic `ui_action` Pending And Result Feedback

## Context And Constraints

Ticket: `ticket_1777662613_250198` - Add generic UI action pending and result feedback.

The current browser `ui_action` path sends a semantic `UiAction` from `app/frontend/ui_contract/dispatch.ts` through `HubTransport.sendCommand("ui_action", ...)`. The hub receives it in `cli/lua/handlers/commands.lua` and dispatches through `cli/lua/lib/action.lua`. Today the browser only knows whether the encrypted command was sent, not whether the action was handled, failed validation, raised, or completed.

Hard constraints that shape the implementation:

- Botster web is a React/Catalyst SPA fed by structured hub frames, not a Rails/Hotwire UI for these surfaces.
- Shared UI primitives stay semantic. No CSS, DOM hints, or web-only behavior should be added to public Lua `UiAction` payload semantics.
- Existing Catalyst/local primitives under `app/frontend/components/catalyst` and `app/frontend/ui_contract/registry.tsx` own button/form rendering.
- `tmp/tailwind_plus_preview` is absent in this worktree (`tmp` only contains `.keep`, `pids/.keep`, and `storage/.keep`), so do not copy a Tailwind Plus preview pattern. Use the vendored Catalyst `Button`, `Fieldset`, `Text`, and existing local primitive classes.
- Cold turkey applies: this becomes the standard `ui_action` lifecycle in one PR. Do not leave plugin-specific pending/result paths as the parallel standard unless there is a real deployment boundary. There is no feature flag.
- CLI verification must use `cd cli && ./test.sh ...`, never raw `cargo test`.

## Consumer Surface Boundary

Primary scope is the React/Catalyst web client that renders hub-authored `UiNode` trees:

- Submitters: `button` and `icon_button` primitives rendered in `app/frontend/ui_contract/registry.tsx`.
- Forms: `form` primitive rendered in `registry.tsx`; submit behavior already flows through button `wrapActionClick(..., { validate: true })` and `reportActionValidity`.
- Dispatch: `createTransportDispatch` in `app/frontend/ui_contract/dispatch.ts`.
- Frame ingress: `HubTransport.handleMessage` in `app/frontend/lib/connections/hub_connection.js`.

Rails ERB/Hotwire pages in `app/views/**` do not emit `ui_action` today. They use ordinary Rails forms/buttons for auth/settings pages, so this ticket should not add Stimulus or Hotwire code. If implementation discovers an ERB `ui_action` submitter, scope Stimulus to data attributes only and keep visuals in Tailwind/Catalyst-compatible markup.

## Wire Shape

Keep one existing outbound command type:

```json
{
  "type": "ui_action",
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "envelope": {
    "id": "project_pipelines.create_ticket",
    "payload": { "project_id": "..." }
  }
}
```

Add one inbound result frame:

```json
{
  "type": "ui_action_result",
  "v": 1,
  "subscriptionId": "<same subscription id>",
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "action_id": "project_pipelines.create_ticket",
  "ok": true,
  "handled": true,
  "via": "handler",
  "message": "Ticket created"
}
```

Error shape:

```json
{
  "type": "ui_action_result",
  "v": 1,
  "subscriptionId": "<same subscription id>",
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "action_id": "project_pipelines.create_ticket",
  "ok": false,
  "handled": true,
  "via": "handler",
  "error": "Select a spawn target before creating the ticket."
}
```

Field ownership:

- `action_request_id` is generated browser-side immediately before send so the same id can mark local pending state before transport work starts.
- `v: 1` is protocol versioning and is allowed by the wire convention. Do not use source names like `ui_action_result_v1`.
- `message` and `error` are optional. Hub/Lua handlers may return structured result metadata, but existing handlers that only return `action.HANDLED` get generic success/failure derived by `lib.action`.
- No outbound `ui_action_pending` frame is needed. Pending is local browser state because it starts at submit time and should not wait for a hub round trip.
- No existing wire type is deprecated. This extends the existing `ui_action` command with additive metadata and adds `ui_action_result`.

Lost-response behavior:

- Browser marks request pending before `sendCommand`.
- If `sendCommand` returns `false` or throws, clear pending and store a local error result immediately.
- If no `ui_action_result` arrives within a bounded timeout, clear pending and store a timeout error. Suggested default: 15 seconds, long enough for normal hub work but short enough to prevent stuck disabled buttons.
- Late results after timeout are ignored if the request id is no longer active.

## Implementation Sequence

1. Add the browser lifecycle store.
   - New file: `app/frontend/ui_contract/action_lifecycle_store.ts` or equivalent colocated module.
   - Track `pending` and latest `result` by `action_request_id`.
   - Track enough submitter identity to disable only the clicked submitter. Use request id plus action id/element source; do not globally disable every same-id action unless the submitter cannot be distinguished.
   - Include timeout cleanup and a test-only reset helper.

2. Extend `createTransportDispatch`.
   - Generate `action_request_id`.
   - Mark lifecycle pending before sending.
   - Send the id as command metadata, not inside `UiAction.payload`.
   - On send failure or thrown transport error, record `ok: false` with a transport error and clear pending.
   - Preserve local-only actions. They should not get hub lifecycle state because they do not use `ui_action`.

3. Render pending state in existing primitives.
   - Update `renderButton` and `renderIconButton` in `app/frontend/ui_contract/registry.tsx`.
   - Keep Catalyst/local styling. Use `disabled`, `aria-busy`, `data-ui-action-pending`, and existing size/layout classes.
   - For text buttons, show an inline non-layout-shifting pending affordance next to the label. Prefer existing icon infrastructure (`IconGlyph`) if an existing spinner/loading glyph is available; otherwise use CSS animation on a small element without introducing unicode glyphs.
   - Forms inherit this through the submit button path and native validity checks.

4. Route `ui_action_result` in the browser.
   - Add a case in `app/frontend/lib/connections/hub_connection.js`.
   - Either call the lifecycle store directly or emit a specific `uiActionResult` event consumed once by the store. Do not let it fall through to generic `"message"`.
   - Keep `transient_event` separate. This ticket is not a toast workaround.

5. Emit generic results from Lua.
   - In `cli/lua/handlers/commands.lua`, pass `action_request_id` and `target_surface` into the dispatch context.
   - In `cli/lua/lib/action.lua`, preserve observer semantics and fallback behavior, but collect outcome metadata:
     - `handled`, `via`, `handler_count`, `handled_count`.
     - `ok: false` when a handler raises or fallback dispatch fails.
     - Optional `message`/`error` from a returned table, without treating arbitrary truthy returns as handled.
   - After dispatch, if `ctx.client` and `ctx.action_request_id` exist, send `ui_action_result` on the same subscription.
   - Invalid envelope should also send `ok: false` when a request id exists.

6. Retrofit existing ad-hoc feedback in the same PR.
   - Project Pipelines plugin currently stores per-client `actions.feedback(ctx)` state and renders result notices/errors in:
     - `catalog/templates/plugins/project-pipelines/project_pipelines/web/actions.lua`
     - `catalog/templates/plugins/project-pipelines/project_pipelines/web/screens/new.lua`
     - `catalog/templates/plugins/project-pipelines/project_pipelines/web/screens/ticket.lua`
   - Convert one-shot submit outcomes to generic `ui_action_result` messages:
     - `create_ticket`: missing target is an error result; successful create returns message plus enough payload for optional navigation if needed.
     - `create_project`: success result replaces `created_project_id` notice panel.
     - `add_ticket_dependency` / `remove_ticket_dependency`: errors and success use action results instead of `dependency_error`.
     - `request_merge`: success/error use action results instead of `merge_notice` / `merge_error`.
     - `close_ticket`: error uses action result instead of `close_error`.
     - `answer_question`: error uses action result instead of `question_error`.
     - Field update actions may keep inline field validation if they are durable form state, but transient field operation errors should return action result metadata where applicable.
   - Remove the replaced one-shot feedback keys and render panels in the same change. Do not leave comments that describe the old path as temporary/deprecated.
   - Other non-`ui_action` local UI feedback, such as pairing/share/settings forms, remains out of scope because it is not part of the `ui_action` lifecycle.

7. Documentation.
   - Update `docs/lua/primitives.md` near the shared form/action section with the `action_request_id` and `ui_action_result` lifecycle.
   - Update comments in `cli/lua/lib/action.lua` to describe result semantics without version-suffix naming.

## Rollback Posture

No feature flag and no dual path. The PR should be internally ordered so each commit is coherent, but the final diff must have one standard lifecycle:

1. Browser can send correlated `ui_action`.
2. Hub can answer correlated results.
3. Browser consumes results and renders pending/result state.
4. Existing Project Pipelines ad-hoc one-shot feedback is removed or converted.
5. Tests prove the whole loop.

If a mid-PR commit temporarily has browser pending without hub result, that is acceptable only inside the implementation sequence, not as the merge state.

## Test Plan

Frontend Vitest:

- `app/frontend/ui_contract/__tests__/dispatch.test.tsx`
  - Adds `action_request_id` to outbound `ui_action` command metadata.
  - Marks pending before async send resolves.
  - Clears pending and records transport error when `sendCommand` returns `false`.
  - Clears pending and records transport error when `sendCommand` throws.
  - Does not lifecycle-track `LOCAL_ONLY_ACTIONS`.
  - Does not dispatch disabled actions.

- `app/frontend/ui_contract/__tests__/primitives.test.tsx`
  - `button` renders `disabled` and `aria-busy` when its request is pending.
  - pending state prevents accidental double click for the same submitter.
  - `icon_button` exposes pending/busy state without changing its accessible label.
  - form validation still blocks dispatch before pending starts.

- `app/frontend/test/ui-tree.test.jsx`
  - Connected-client check: click a primitive button, observe pending state, inject a `ui_action_result` message, assert pending clears and result is visible without a new `ui_tree_snapshot`.
  - Ensure stale tree/surface switch cleanup does not leave pending state active for the old transport.

- `app/frontend/test/hub-connection-request-send-rejected.test.js` or a new `app/frontend/test/ui-action-result-frame.test.js`
  - `HubTransport.handleMessage` routes `ui_action_result` to the lifecycle path and does not emit generic `"message"`.

CLI/Lua tests through `cli/test.sh`:

- Add or extend `cli/tests/internal_client_lua_test.rs`.
  - Dispatch `{ type = "ui_action", action_request_id = "req-1", envelope = ... }` through `lib.internal_client`.
  - Assert exactly one frame `{ type = "ui_action_result", v = 1, action_request_id = "req-1", ok = true }`.
  - Assert invalid envelope with request id returns `ok = false`.

- Add a focused `cli/tests/ui_action_lifecycle_lua_test.rs` if the test shape grows beyond internal client coverage.
  - Handler returning `action.HANDLED` sends handled success result.
  - Handler returning a result table can supply `message` or `error`.
  - Handler exception is isolated and produces `ok = false` while other handlers still run.
  - Fallback route for `botster.session.select` still reports `via = "fallback"`.

Commands to run:

```bash
npm run test:frontend -- app/frontend/ui_contract/__tests__/dispatch.test.tsx app/frontend/ui_contract/__tests__/primitives.test.tsx app/frontend/test/ui-tree.test.jsx app/frontend/test/ui-action-result-frame.test.js
cd cli && ./test.sh --integration -- internal_client_lua_test ui_action_lifecycle
```

If the CLI filter cannot select both tests cleanly, run:

```bash
cd cli && ./test.sh --integration -- ui_action
```

End-to-end check:

- Use the internal client test as the hub-loop proof and the `ui-tree` Vitest as the connected browser proof: submitter pending starts locally, a correlated result frame arrives on the same connected transport, pending clears, and success/error is visible without requiring a fresh tree snapshot.

