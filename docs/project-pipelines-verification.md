# Project Pipelines Verification

## 2026-05-04 Workerized Architecture Docs And Static Boundary Checks

Scope:

- `docs/worker-actor-contracts.md` aligned with production worker ownership
  instead of aspirational full-worker claims.
- WebRTC receiver ownership guarded by source-backed static tests: production
  code must use registry-owned queue tasks and typed adapter/control events,
  while raw receiver polling/leasing stays `#[cfg(test)]`.
- Browser WebRTC terminal subscribe/unsubscribe/focus ingress guarded so typed
  terminal controls cross the browser `ClientWorker` boundary instead of
  falling back to adapter-only or Lua routing.
- Durable-session PTY bytes are explicitly outside hub hot-path events:
  WebRTC/TUI/socket terminal streams route through `ClientWorker` and
  `SessionIoWorker`; hub policy owns authorization, attach correlation, and
  cleanup.
- Session I/O documentation guarded against claiming mailbox ownership for
  `SessionIoRequest` variants unless the runtime has executable handling.
- `BoundaryJson` guarded as a Lua/plugin/relay boundary exception, with the
  current WebRTC subscribe acknowledgement bridge documented explicitly.

Commands and results:

```bash
cd cli && ./test.sh --unit -- worker
# 76 passed; 0 failed

cd cli && ./test.sh --unit -- server_comms
# 59 passed; 0 failed

cd cli && ./test.sh --unit
# 1531 passed; 0 failed; 1 ignored
```

Notes:

- CLI verification used `cli/test.sh`, not raw `cargo test`, so
  `BOTSTER_ENV=test` was set for all Rust/Lua slices.
- Static boundary checks live in `worker::tests` and read the relevant source
  and docs with `include_str!`, replacing brittle ad hoc field-name scans with
  semantic ownership assertions.
- `WebRtcPeerRegistry::poll_received_messages` is now test-only, matching its
  only remaining hub usage under `poll_webrtc_peer_payloads_for_tests`.
- No UI surface changed; `tmp/tailwind_plus_preview` and Catalyst/Elements
  review were not applicable.

## 2026-05-04 Workerized Session I/O Timing Regression Verification

Scope:

- Deterministic workerized `SessionIoRuntime` output coalescing tests.
- Explicit 4 ms age-based flush, 16-frame flush, and 32 KiB flush coverage.
- Bell, OSC notification, and process-exit ordered flush boundaries.
- Client-worker delivery assertions for coalesced terminal chunks without
  routing durable-session PTY bytes through hub event handlers.

Commands and results:

```bash
cd cli && ./test.sh --unit -- session_io_runtime
# 16 passed; 0 failed

cd cli && ./test.sh --unit -- server_comms
# 55 passed; 0 failed

cd cli && ./test.sh --unit -- worker
# 60 passed; 0 failed

cd cli && ./test.sh --unit
# 1516 passed; 0 failed; 1 ignored

rg -n "sleep\\(Duration::from_millis|thread::sleep|tokio::time::sleep" cli/src/worker/session_io_runtime.rs cli/src/hub/server_comms.rs
# Remaining session_io_runtime sleep matches are sibling tests intentionally
# left out of this ticket's scope per plan review; server_comms matches are
# unrelated existing retry/backoff and async fixture waits.

rg -n "terminal_subscription|FRAME_BELL|FRAME_NOTIFICATION|FRAME_PROCESS_EXITED|output_age_flushes|output_thresholds_flush|output_flushes_before" cli/src/worker/session_io_runtime.rs cli/src/hub/server_comms
# Confirmed new regression coverage is wired to the worker output thresholds,
# structured frame boundaries, and client subscription delivery.
```

Notes:

- CLI verification used `cli/test.sh`, not raw `cargo test`, so
  `BOTSTER_ENV=test` was set for all Rust/Lua slices.
- The touched burst/coalescing tests assert bounded worker delivery rather than
  reintroducing a hub PTY-byte batch path.
- The explicit 4 ms test keeps the stream open and stays below the 16-frame and
  32 KiB thresholds, so it exercises the age-triggered flush path without EOF.
- No UI surface changed; `tmp/tailwind_plus_preview` was not applicable.

## 2026-05-02 TUI/Socket Client Worker Transport Verification

Scope:

- TUI and local socket terminal streams moved onto the transport-neutral
  `ClientWorker` data plane.
- Lossless typed egress for PTY bytes, scrollback metadata, process exit, JSON
  control frames, and plugin binary frames.
- Hub-owned attach/detach lifecycle driven by worker hub-control messages.
- Cold-turkey removal of covered TUI/socket direct terminal delivery paths.

Commands and results:

```bash
cd cli && ./test.sh --unit -- worker
# 38 passed; 0 failed

cd cli && ./test.sh --unit -- server_comms
# 39 passed; 0 failed

cd cli && ./test.sh --unit -- socket
# 74 passed; 0 failed

cd cli && ./test.sh --unit -- tui_bridge
# 10 passed; 0 failed

cd cli && ./test.sh --unit
# 1478 passed; 0 failed; 1 ignored
```

Notes:

- CLI verification used `cli/test.sh`, not raw `cargo test`, so
  `BOTSTER_ENV=test` was set for all Rust/Lua/TUI slices.
- The socket-worker live-output parity test now passes under the standard
  focused filters with default test concurrency; it is not dependent on
  `--test-threads=1`.
- Merge-agent verification was rerun after integrating `origin/main`, including
  the WebRTC client-worker transport adapter changes already on main.
- Static boundary checks confirmed `cli/src/worker/client.rs` has no concrete
  socket, TUI bridge, or WebRTC imports. Remaining `send_frame` matches are
  outside the covered TUI/local-socket terminal subscription path.
- No browser UI surface was changed. `tmp/tailwind_plus_preview` was absent in
  this worktree, and no Catalyst/Elements comparison was required.

## 2026-05-02 Plugin Entity End-to-End Verification

Scope:

- Plugin entity requested snapshots and targeted deltas.
- Browser and TUI consumption of the same entity stream.
- Row-scoped `ui_action_result` pending feedback cleanup.
- Cold-turkey removal of Project Pipelines dynamic-list dependency on forced
  `ui_tree_snapshot` refresh helpers.

Commands and results:

```bash
cd cli && ./test.sh --unit -- ui_contract
# 196 passed; 0 failed

cd cli && ./test.sh --integration -- project_pipelines
# 4 passed; 0 failed

cd cli && ./test.sh --integration -- emits_entity
# 3 passed; 0 failed

cd cli && ./test.sh --integration -- send_snapshots_to
# 2 passed; 0 failed

cd cli && ./test.sh --integration -- table_renders_rows_from_plugin_entity_bind
# 1 passed; 0 failed

cd cli && ./test.sh --integration -- ui_action_dispatch_emits_correlated_success_result
# 1 passed; 0 failed

npm test -- app/frontend/test/entity-stores.test.js app/frontend/test/binding.test.ts app/frontend/test/ui-action-result-frame.test.js app/frontend/test/ui-tree.test.jsx
# 4 files passed; 58 tests passed; 0 failed

rg -n "broadcast_ui_tree_snapshots|send_ui_tree_snapshots" catalog/templates/plugins/project-pipelines cli/src app/frontend
# no matches

rg -n "ui\.bind_list|ui\.bind\(\"/project-pipelines" catalog/templates/plugins/project-pipelines/project_pipelines/web
# Project Pipelines home and project screens bind dynamic rows to /project-pipelines.* entity stores
```

Notes:

- CLI verification used `cli/test.sh`, not raw `cargo test`, so
  `BOTSTER_ENV=test` was set for all Rust/Lua/TUI slices.
- No Rails API or model files were touched, so Rails tests were not applicable.
- No visible UI surface was changed. `tmp/tailwind_plus_preview` was absent in
  this worktree, and no Catalyst/Elements comparison was required.

## 2026-05-02 Plugin Entity Documentation Verification

Scope:

- Plugin-owned entity architecture documentation.
- Lua primitive reference and UI contract docs.
- Project Pipelines authoring guide and entity-backed UI case study.

Static checks performed:

```bash
rg -n "entity_snapshot|entity_upsert|entity_patch|entity_remove|bind_list|ui_action_result|action_request_id" docs cli/src/ui_contract catalog/templates/plugins/project-pipelines
rg -n "browser-only|plugin-specific list snapshot|template cloning|legacy|v1|v2|ui_tree_snapshot" docs cli/src/ui_contract catalog/templates/plugins/project-pipelines
rg -n "lib.entity_broadcast|entity_snapshot|ui.bind_list|action.result" catalog/templates/plugins/project-pipelines cli/lua
```

Results:

- Required entity, binding, and action lifecycle terms appear in the new
  canonical guide plus Lua, UI contract, web runtime, and Project Pipelines docs.
- Any remaining `legacy` matches are implementation cleanup identifiers such as
  `pipeline.legacy_prune_checked`, not a documented plugin UI compatibility
  mode.
- `ui_tree_snapshot` matches should describe presentation/control structure,
  not a durable plugin model-state channel.
- All documented paths for this pass were present:
  `docs/plugin-entities.md`, `docs/lua/primitives.md`,
  `docs/lua/session-actions.md`, `cli/src/ui_contract/README.md`,
  `docs/specs/cross-client-ui-primitives.md`,
  `docs/specs/web-ui-primitives-runtime.md`,
  `catalog/templates/plugins/project-pipelines/README.md`,
  `docs/project-pipelines-verification.md`, `README.md`, and
  `docs/lua-architecture-vision.md`.

Project Pipelines remains the reference plugin for entity-backed dynamic state
and generic action lifecycle feedback.

## 2026-05-21 Pipeline Versioning And Archive Support

Implementation scope:

- Added nullable pipeline lifecycle metadata: `version_label`, `archived_at`,
  `replacement_pipeline_id`, and `supersedes_pipeline_id`.
- Active/default selection paths omit archived definitions while direct repo
  lookups still resolve historical runs by pipeline id.
- MCP list/get expose explicit `include_archived` behavior; update accepts
  archive, version, and replacement metadata.
- Home and pipeline index lists filter `/project-pipelines.pipeline` rows by
  active state while direct edit/detail paths can still hydrate archived rows.
- The explicit archived pipeline view at `/pipelines/archived`
  exposes retired definitions without changing default active-only lists.

Verification commands:

```bash
cd cli && ./test.sh --unit -- project_pipelines
cd cli && ./test.sh --integration -- project_pipelines
rg -n "title.*prefix|prefix.*title" catalog/templates/plugins/project-pipelines/project_pipelines
rg -n "archived_at|version_label|replacement_pipeline_id|supersedes_pipeline_id|/pipelines/archived" catalog/templates/plugins/project-pipelines/project_pipelines
```

Expected proof:

- Integration coverage should prove schema/allow-list wiring, MCP
  `include_archived` behavior, `engine.start_run` rejecting explicit archived
  pipelines, repo default/list filtering, ticket history name preservation,
  entity active/archived queries, archived UI meta rendering, v10 migration, and
  replacement-link validation.
- Static inspection should find archive/version metadata fields and no
  title-prefix lifecycle logic in Project Pipelines Lua files.

Results:

- `cd cli && ./test.sh --unit -- project_pipelines` passed, but matched zero
  library tests because Project Pipelines coverage is in integration tests.
- `cd cli && ./test.sh --integration -- project_pipelines` passed. The filtered
  run included 42 Project Pipelines plugin tests plus related catalog checks.
- The title-prefix scan returned no Project Pipelines Lua matches.
- The metadata field scan found the schema, repo, MCP, entity contract, web UI,
  explicit archived route, and engine archive/version wiring.

## 2026-05-01 Dependency And Merge Polish

Manual verification was run against the live `~/.botster-dev/plugins/project-pipelines` plugin after reloading the plugin and layout.

Scope:

- Ticket dependency CRUD.
- Dependency blocker enforcement before run start.
- Dependency cleanup when deleting tickets.
- Cycle rejection.
- Template catalog parity for Project Pipelines.

Results:

- Created throwaway tickets `ticket_1777675926_812500` and `ticket_1777675938_308597`.
- Added dependency `ticket_1777675926_812500 -> ticket_1777675938_308597`; `project_pipelines_list_ticket_dependencies` returned the dependency with `depends_on_status = "open"`.
- Attempted self-dependency `ticket_1777675926_812500 -> ticket_1777675926_812500`; rejected with `ticket cannot depend on itself`.
- Attempted cycle `ticket_1777675938_308597 -> ticket_1777675926_812500`; rejected with `ticket dependency would create a cycle`.
- Attempted `project_pipelines_start_run` on dependent ticket while dependency was open; rejected before pipeline lookup with `ticket dependencies must close before starting a run: Verification dependency B (open)`.
- Deleted dependency ticket `ticket_1777675938_308597`; dependency lists for `ticket_1777675926_812500` returned empty results, proving dependency rows were purged.
- Retried `project_pipelines_start_run` with a deliberately missing pipeline; error changed to `pipeline not found`, proving dependency enforcement was no longer blocking.
- Deleted all throwaway verification tickets, including earlier cleanup tickets `ticket_1777675710_491621` and `ticket_1777675721_427180`.

Static and catalog checks:

- `luac -p` passed for live and template Project Pipelines Lua files.
- `cli/./test.sh --integration -- project_pipelines_template_catalog_entry_is_a_multi_file_plugin` passed.

Code inspection:

- `project_pipelines_close_ticket` now accepts `merge_commit`, `pr_url`, and `merge_summary` through MCP, threads them through `engine.close_ticket`, and writes a `kind = "merge"` artifact before closing when `merge_confirmed = true`.
- Returning to an existing step agent sends both `Hub:post(... type = "task")` and `Hub:notify(...)` pointing the agent to `project_pipelines_current_context`.
