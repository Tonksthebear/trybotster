# Plan: Hub Cloudflare Connector Lifecycle

Ticket: `ticket_1779931231_383179`
Run: `run_1780027377_261319`
Step: `botster_plan`

## Context Loaded

- Pipeline context loaded with `project_pipelines_current_context` for `run_1780027377_261319`.
- Current ticket: Build hub Cloudflare connector lifecycle.
- Current gate: `botster_plan_gate`.
- Prior Plan Review verdict: changes required because the first plan was not persisted as a reviewable artifact and needed sharper coverage for broker lineage, production entry point, named-tunnel command preparation, secret non-exposure, quick-preview coexistence, and reload/restart reconciliation.
- No open human questions or prior answers.
- Dependencies are marked closed:
  - `ticket_1779931218_799029`: Design stable URL and webhook ingress contracts.
  - `ticket_1779931226_656587`: Implement Rails Cloudflare tunnel broker.

Vault and repo context loaded:

- `~/knowledge/notes/planner-playbook.md`
- `~/knowledge/notes/botster-planner-playbook.md`
- `~/knowledge/notes/botster-architecture.md`
- `~/knowledge/notes/cli-patterns.md`
- `~/knowledge/notes/spa-patterns.md`
- `~/knowledge/notes/project pipeline orchestration belongs in a device-level botster plugin.md`
- `~/knowledge/notes/project pipelines needs an operator workbench not more primitives.md`
- `~/knowledge/notes/project pipelines ui contract belongs in the plugin readme.md`
- `~/knowledge/notes/botster orchestration should spawn agents with explicit target ids.md`
- `~/knowledge/notes/botster orchestration prompts must bind agents to explicit worktrees.md`
- `~/knowledge/notes/stable url claims should be a shared plugin resource.md`
- `~/knowledge/notes/plugin stable webhook urls need a generic ingress contract.md`
- `~/knowledge/notes/cloudflared named tunnels route multiple hostnames via config.yml ingress array.md`
- `~/knowledge/notes/cloudflare tunnel token rotation is update secret plus token fetch.md`
- `~/knowledge/notes/cloudflare recovered connector sessions rebuild parent preview action state.md`
- `~/knowledge/notes/hosted preview recovery must include hidden owned connector sessions.md`
- `docs/specs/stable-webhook-url-contracts.md`
- `docs/lua/core-product-boundaries.md`
- `docs/plugin-entities.md`
- `CLAUDE.md`

Repo observations:

- Current checkout is based on `project-pipelines/ticket_1779931218_799029` at `050e7b9a`.
- Current checkout has the stable contract doc, the existing quick-preview plugin at `catalog/templates/plugins/cloudflare-hosted-preview/init.lua`, the secrets primitive at `cli/src/lua/primitives/secrets.rs`, and quick-preview tests at `cli/tests/cloudflare_hosted_preview_lua_test.rs`.
- Current checkout does not include the closed Rails broker dependency in its base lineage. The dependency branch `project-pipelines/ticket_1779931226_656587` contains commit `84e8edb2` with Rails broker models/controllers/tests; Plan Review also named PR/commit `#196` / `71d3a18a` as the broker lineage to verify.

## Scope

In scope:

- Base this implementation on a tree that includes the Rails Cloudflare broker code. The implementer must either rebase/merge onto a base containing `#196` / `71d3a18a` or otherwise bring in the closed broker dependency before wiring the hub plugin.
- Add a hub-scoped Cloudflare stable URL connector plugin, recommended as `catalog/templates/plugins/cloudflare-stable-urls/`.
- Keep connector lifecycle plugin-owned and executed through the plugin worker boundary. Core Lua/Rust may expose only generic primitives or narrow helpers when an existing primitive is insufficient.
- On plugin load/reload and hub restart, run a production reconciliation entry point from the stable connector plugin. That path must compare Rails desired state, plugin state or plugin.db claims, secret presence, generated token/config files, and live connector sessions before spawning anything.
- Request brokered tunnel material from Rails through the hub-authenticated broker endpoints.
- Store `connector_token` immediately through the existing `secrets` primitive. Persist only secret pointers, token version metadata, timestamps, claim owner fields, and non-secret status.
- Materialize a token file safely for `cloudflared`.
- Run exactly one hub-level `cloudflared` connector for the named tunnel.
- Use the current broker token mode: do not generate or pass a local config.yml ingress file unless Rails later switches this contract to locally managed credentials-file mode.
- Publish provider and claim state through plugin entities, using the documented family `cloudflare-stable-urls.stable_url` or an explicitly documented equivalent.
- Handle connector process exit by publishing non-secret unhealthy/reconciling state and allowing the reconcile path to restart once safe.
- Preserve existing `cloudflare-hosted-preview` quick tunnel behavior. The stable connector coexists with quick preview for this ticket.

Non-scope:

- Do not implement provider webhook parsing, GitHub signature verification, replay/idempotency semantics, or durable provider business actions.
- Do not implement arbitrary path multiplexing under one hostname.
- Do not move Cloudflare account API credentials into the CLI or Lua plugin. Rails remains the Cloudflare account credential owner.
- Do not replace quick-preview behavior in this ticket.
- Do not build a broad new orchestration framework, workbench UI, or SPA/TUI redesign.
- Do not turn the stable contract spec into the only deliverable. This ticket must wire the actual runtime path that starts and reconciles the connector.

## Assumptions And Unknowns

Assumptions:

- The closed Rails broker work is the intended source for `/hubs/:hub_id/cloudflare_tunnel` and `/hubs/:hub_id/stable_webhook_hostnames`.
- The implementation should rebase/merge onto the broker lineage rather than treat the broker as a deployed-only HTTP contract, because this repo has the Rails app and tests in-tree.
- Existing `secrets` storage is the correct token storage mechanism.
- Existing command preparation/accessory creation primitives should be reused where possible, but adapted to hub-scoped ownership rather than parent-session ownership.
- Quick-preview and stable connector plugins coexist. Quick preview remains per-session and quick-tunnel based; stable connector is hub-level and named-tunnel based.

Unknowns to resolve during implementation:

- Which existing Lua helper exposes the hub bearer token or base Rails URL to plugin worker code. If none exists, add the smallest helper at the existing hub/Rails boundary.
- Whether current primitives can safely materialize the token file with restricted permissions. If not, add a narrow, test-covered primitive/helper rather than leaking token material through generic logs or entities.
- Whether the stable connector should represent its process as a hidden accessory session or a more direct plugin-owned process. The plan prefers using existing hidden accessory/session ownership if it can be made hub-scoped and idempotent.
- Exact plugin.db schema. Keep it minimal: connector record, claims, generations, local listener URL/port, live connector session UUID, status, message, and timestamps.

## Affected Surfaces And Files

Likely files:

- `catalog/templates/plugins/cloudflare-stable-urls/init.lua`
- `catalog/templates/plugins/cloudflare-stable-urls/cloudflare_stable_urls/db.lua`
- `catalog/templates/plugins/cloudflare-stable-urls/cloudflare_stable_urls/repo.lua`
- `catalog/templates/plugins/cloudflare-stable-urls/cloudflare_stable_urls/entities.lua`
- `catalog/templates/plugins/cloudflare-stable-urls/cloudflare_stable_urls/entity_contract.lua`
- `cli/tests/cloudflare_stable_urls_plugin_test.rs`
- `cli/lua/lib/hub.lua` or an existing hub API helper, only if required for authenticated Rails broker calls.
- `cli/src/lua/primitives/secrets.rs` only if tests show token-file materialization cannot be done safely with existing primitives.
- Rails broker files from the closed dependency, expected in the implementation base:
  - `app/models/hubs/cloudflare_tunnel.rb`
  - `app/models/hubs/stable_webhook_hostname.rb`
  - `app/controllers/hubs/cloudflare_tunnels_controller.rb`
  - `app/controllers/hubs/stable_webhook_hostnames_controller.rb`
  - `app/models/cloudflare/tunnel_api.rb`
  - related migrations, routes, fixtures, and tests.
- Docs only if implementation changes the documented contract:
  - `docs/specs/stable-webhook-url-contracts.md`
  - `docs/lua/core-product-boundaries.md`
  - `docs/plugin-entities.md`

Surfaces to preserve:

- `catalog/templates/plugins/cloudflare-hosted-preview/init.lua`
- `cli/tests/cloudflare_hosted_preview_lua_test.rs`
- frontend quick-preview action rendering tests that currently expect `cloudflare.preview.toggle`.

Botster layers touched:

- Lua plugin/template
- Plugin worker runtime boundary
- Existing Rust/Lua primitives only if a narrow missing helper is proven
- Rails broker dependency integration
- Plugin entity publication
- CLI Lua/plugin tests

## Implementation Shape

1. Integrate broker lineage.
   - Verify `git merge-base --is-ancestor 71d3a18a HEAD` or equivalent broker files are present.
   - If absent, rebase/merge the closed broker dependency before implementing the connector.
   - Run the focused Rails broker tests after integration.

2. Add stable connector plugin.
   - Use plugin-owned modules matching the entity-backed plugin pattern: `db.lua`, `repo.lua`, `entities.lua`, and `entity_contract.lua`.
   - Register/replay entity providers on plugin load.
   - Register a plugin load/reload reconciliation call from `init.lua`, for example `connector.reconcile("plugin_load")`.
   - Subscribe to `process_exited` and plugin/session recovery events needed to repair connector state.

3. Broker material fetch.
   - On reconcile, call Rails broker `POST /hubs/:hub_id/cloudflare_tunnel` when no active local token generation exists or when Rails desired state changed.
   - Store returned `connector_token` immediately with `secrets.set("cloudflare-stable-urls", token_key, token)`.
   - Do not persist raw token in plugin.db, entity rows, logs, or command metadata.

4. Token materialization.
   - Materialize a token file from the secret into a plugin runtime data directory, not the plugin source tree.
   - Ensure restricted permissions on Unix, expected `0600`.
   - Do not generate a local `config.yml` for the broker's remotely managed connector-token mode.
   - Never include raw token in command metadata, plugin.db, entities, or generated runtime files other than the token file itself.

5. Connector command.
   - Prepare a named-tunnel command equivalent to `cloudflared tunnel run --token-file <file> <tunnel>`.
   - Command must not include `--url` or `--config`.
   - Command metadata must include owner plugin, system kind, connector generation, and observe/process-exit tracking, but no token bytes.

6. Single-connector reconciliation.
   - Reconcile must list owned hidden connector sessions, not just visible sessions.
   - If zero live connectors and broker/secret/token file are valid, spawn one.
   - If one live connector matches current generation, publish healthy/running state.
   - If multiple connectors exist, keep the current-generation connector and close stale connectors.
   - If a connector belongs to an older generation, fence its late output/process-exit completion so it cannot overwrite current state.
   - On plugin reload or hub restart, rebuild provider entity state from durable plugin.db plus connector metadata.

7. Quick-preview coexistence.
   - Do not alter `cloudflare-hosted-preview` behavior except for imports/shared helpers if unavoidable.
   - Keep quick-preview tests passing.

## Risks

- Broker-not-in-lineage risk: implementation can become unwired if the Rails broker dependency is not present in the final base.
- Secret leakage risk: raw connector tokens can leak through plugin.db, entity frames, command metadata, logs, test fixtures, generated config, or process environment.
- Named-tunnel/quick-tunnel confusion: `--url` on a named tunnel bypasses config ingress.
- Duplicate connector risk after plugin reload or hub restart if reconciliation does not inspect owned hidden connector sessions and generation metadata.
- Restart loop risk if missing `cloudflared`, bad token, or invalid config triggers immediate respawn instead of publishing an actionable error.
- Provider state risk: entity publication can claim health without proving the production plugin load/reconcile path ran.
- Management-mode risk: the broker currently returns a remotely managed connector token, so this implementation must not rely on local config.yml ingress; locally managed credentials-file ingress is a separate design.

## Acceptance Checks And Tests

Broker lineage and Rails tests:

- `git merge-base --is-ancestor 71d3a18a HEAD` or an equivalent command proving the broker dependency is in the final base.
- `bin/rails test test/controllers/hubs/cloudflare_tunnels_controller_test.rb test/controllers/hubs/stable_webhook_hostnames_controller_test.rb test/models/cloudflare/tunnel_api_test.rb test/models/hubs/cloudflare_tunnel_test.rb test/models/hubs/stable_webhook_hostname_test.rb`

Stable connector focused CLI tests:

- Use the repo-approved harness, not raw `cargo test`:
  - `cd cli && ./test.sh --integration -- cloudflare_stable_urls`
- Test missing `cloudflared`:
  - simulated `prepare_plugin_command` returns `command_missing`
  - plugin publishes non-secret error and install URL
  - no connector spawn loop starts
- Test token storage:
  - Rails broker response includes sentinel token
  - plugin calls `secrets.set`
  - plugin.db rows and entity payloads contain only secret key/pointer and token version, never the sentinel token
- Test token-file materialization:
  - token file is written under runtime plugin data, not plugin source
  - Unix permissions are `0600`
  - command metadata does not contain the sentinel token
- Test connector command preparation:
  - command args do not include `--url`
  - command args do not include `--config`
  - command args include `--token-file` and the materialized token-file path
- Test exactly-one connector reconciliation:
  - zero live connectors spawns one
  - one current connector reuses it
  - multiple owned connectors close stale generations and keep one
  - hidden owned connector sessions are included in discovery
- Test process exit:
  - current-generation exit marks provider/claims `unhealthy` or `reconciling`
  - stale-generation exit is fenced and cannot overwrite current provider state
  - restart happens only through reconcile, not an unbounded immediate loop
- Test reload/restart:
  - plugin reload reruns reconcile and does not duplicate connectors
  - hub restart recovery rebuilds provider entity state from connector metadata/plugin.db
  - late stale completions are ignored by generation check
- Test provider state publication:
  - `cloudflare-stable-urls.stable_url` entity snapshot/upsert is emitted from the production plugin load/reconcile path
  - entity contains public URL/status/owner/local service/token version metadata
  - entity omits raw token, token secret key if considered sensitive, raw config contents, Cloudflare account credentials, and process environment

Quick-preview regression:

- `cd cli && ./test.sh --integration -- catalog_plugin_cloudflare`
- Retained behavior must include registration of `cloudflare.preview.toggle`, missing-cloudflared handling, quick URL parsing/readiness, process exit handling, and existing reload recovery.

Static guardrails:

- `rg -n "connector_token|token_secret|token_secret_key|cfargotunnel|cloudflare" catalog/templates/plugins/cloudflare-stable-urls cli/tests/cloudflare_stable_urls_plugin_test.rs docs`
- Check every match manually for secret leakage or fake-token fixture leakage.
- `rg -n -- "--url" catalog/templates/plugins/cloudflare-stable-urls cli/tests/cloudflare_stable_urls_plugin_test.rs`
- Stable connector implementation should not contain `--url` except in a negative assertion or explanatory test string.

Runtime-path proof required from implementer:

- Name the production plugin entry point that calls reconcile on load/reload.
- Name the production process-exit event handler that updates connector state.
- Show test evidence that those entry points, not only isolated helper functions, drive the behavior.

## Vault Gaps Worth Capturing

- No new durable vault knowledge discovered during planning. Existing notes already cover stable URL claims, generic ingress boundary, Cloudflare named tunnel mechanics, token rotation, plugin entity publication, hidden connector recovery, and the Rails/plugin credential split.
- Run-specific issue to preserve in pipeline evidence, not the vault: this run's current base lacks the closed Rails broker dependency, so implementation must explicitly integrate or verify broker lineage before connector wiring.
