# Plan: Enforce open blocking dependencies at step activation

**Pass:** 2 (addresses Plan Review `review_1786403096_917632`)

## Target

| Field | Value |
| --- | --- |
| `target_id` | `tgt_83619444571645afa5507374e36036e2` |
| `target_repository` | `Tonksthebear/trybotster` |
| `target_path` | monorepo catalog: `catalog/templates/plugins/project-pipelines/` |
| Dogfood install path | `~/.botster-dev/plugins/project-pipelines/` |
| Runtime teardown class | Does not apply |

## Repository playbook loaded

- [[project-pipelines-playbook]] — ownership charter for Project Pipelines engine, schema, MCP, surfaces, and gates

## Other role/surface playbooks and atomic notes loaded

### Role overlays

- [[planner-playbook]]
- [[botster-planner-playbook]]

### Required Botster context maps (from botster-planner-playbook)

- [[botster-architecture]] — domain map; confirms Project Pipelines is a first-party plugin ownership charter and that workflow policy stays in plugins/templates, not core. **Convention conflict: none.**
- [[cli-patterns]] — Rust CLI / Lua plugin runtime patterns; confirms plugin worker + MCP tools are the production path for engine policy. **Convention conflict: none.**

### Product / architecture notes

- [[project pipeline orchestration belongs in a device-level botster plugin]]
- [[project pipelines needs an operator workbench not more primitives]]
- [[project pipelines ui contract belongs in the plugin readme]]
- [[plugin mcp descriptors are the downstream agent contract]]
- [[plan steps need reviewable plan artifacts]]
- [[plan review must verify a plan artifact exists before trusting gate summaries]]
- [[pipeline vault checklists must cite exact resolvable note titles]]
- [[plan agents must author vault context as wikilinks not home paths]]
- [[vault example paths are not repository placement conventions]]

### Dependency-gating notes (evidence discipline)

- [[project pipeline step activation gates open ticket dependencies before side effects]] — **superseded**; do not treat as shipped
- [[vault convention notes can document unimplemented behavior as shipped]] — why the superseded note exists
- [[cleared project pipeline dependencies require explicit reactivation without duplicate spawn requests]] — superseded sibling

### Explicitly not loaded

- [[botster-web-playbook]] — ticket is monorepo catalog plugin policy, not Ionic client shell
- [[botster runtime teardown lenses]] — not WebRTC / SessionIo / peer lifecycle
- Repository crates (`botster-core`, `botster-hub`, TUI, kit, workspaces) — out of ownership for this ticket
- `Projects/botster-project-pipelines` package — ticket forbids treating it as dogfood source of truth

## Plan artifact placement (repository evidence)

Pass 1 used `docs/plans/...`. Refreshed `origin/main` has **no** tracked `docs/plans/` directory.

Repository prior art for Project Pipelines durable docs on main:

- `docs/project-pipelines-verification.md` (tracked under `docs/`)
- other tracked docs live directly under `docs/` or `docs/<area>/`, not `docs/plans/`

Per [[vault example paths are not repository placement conventions]] and [[plan steps need reviewable plan artifacts]], this pass relocates the plan to:

**`docs/project-pipelines-step-activation-dependency-gate.md`**

This matches mainline placement for Project Pipelines documentation. Pass 1 path `docs/plans/...` is retired for this ticket.

## Context loaded

### Ticket intent

REGRESSION / INCOMPLETE FIX relative to closed `ticket_1785989402_277498`.

- `start_run` already refuses open blockers.
- Live evidence shows **step activation inside an already-running run still activated an agent** while a blocking dependency stayed open.
- Fix must live in monorepo built-in plugin:
  - `catalog/templates/plugins/project-pipelines/`
  - install/sync to `~/.botster-dev/plugins/project-pipelines/` after change
- Do **not** paper over with playbook-only instructions.

### Live regression evidence (plugin.db, dogfood)

Observed on `ticket_1785970234_132113` / `run_1786070915_943794`:

| Time | Fact |
| --- | --- |
| `1786072005` | `dependency_1786072005_676257` added: consumer → open kit ticket `ticket_1786071998_949850` |
| `1786072144` | Plan completed; Plan Review activated while kit dependency open |
| `1786072495` | `step.advance_blocked` for **unmet review_clear findings**, not ticket dependencies |
| `1786072603` | Plan Review completed; **Implement activated**; agent queued |
| `1786072603` | `step.completed` evidence itself named the still-open dependency |
| `1786072613` | Implement agent linked |
| `1786072691` | Run cancelled by orchestrator because engine wrongly activated Implement |
| `1786074662` | Kit dependency ticket closed (after the failure) |

Conclusion: the operator-visible failure is real. Relying on `start_run` alone is insufficient once a run is active and dependencies are registered mid-flight.

### Current monorepo code (this worktree / mainline catalog)

`project_pipelines/engine.lua` already contains:

- `unmet_ticket_dependencies(ticket_id)` over `repo.ticket_dependencies`
- `dependency_block(run, attrs)` → `ok=false`, `reason=ticket_dependencies`, `unmet_dependencies`, `step.advance_blocked`
- Call sites:
  - `M.activate_step` before visit creation / notify / spawn
  - `M.request_step_advance` before completing source visit (when `next_step` exists)
  - `M.retry_step_agent` before requeue/spawn
- `M.start_run` still uses `repo.blocking_ticket_dependencies` and raises before create

Unit coverage already exists in `cli/tests/project_pipelines_plugin_test.rs`:

- `catalog_plugin_project_pipelines_start_run_blocks_open_dependencies`
- `catalog_plugin_project_pipelines_public_dependency_write_blocks_advance_before_side_effects`
- retry-path coverage inside `catalog_plugin_project_pipelines_retry_step_agent_requeues_current_visit`

Plugin README already documents the intended activation preflight contract. `init.lua` clears `package.loaded` for plugin modules on load.

### Tension to resolve in Implement

Catalog claims and unit tests assert fail-closed activation. Live dogfood on 2026-08-07 still activated Implement with an open dependency.

Likely residual causes to investigate in order:

1. **Stale dogfood install** — catalog had PR #200 (`07e3230a`) while `~/.botster-dev/plugins/project-pipelines` lagged or was not reloaded for the worker that advanced the run.
2. **Residual production-path gap** — a path still mutates run/step or spawns without going through `dependency_block` (must be proven absent, not assumed).
3. **Source-complete / activation TOCTOU** — `request_step_advance` checks deps, then completes the source visit, then calls `activate_step` again; if the second check fails (or is skipped), the source is already `done` and the response can still look like a successful advance (`ok=true` with nested blocked activation).
4. **Evidence-only fail-open** — unit tests mock `ticket_dependencies`; they do not prove live SQLite join rows + MCP tool path + agent queue refusal against the **same bytes the Hub loaded**.

Implement must treat (1)–(4) as open until closed with proof. Do not close this ticket as “already fixed on main” without identity-pinned live dogfood proof after install/sync and reload.

### Plan Review findings addressed in this pass

| Finding | Class | Disposition |
| --- | --- | --- |
| `finding_1786403096_315985` Live proof does not pin loaded Hub and plugin artifact | product / high | Acceptance now requires source commit, Hub identity, catalog↔installed digests, reload/restart evidence, and public MCP advance/retry event+session proof with no target agent request while open |
| `finding_1786403096_191349` Missing botster-architecture and cli-patterns | process / low | Recorded above; no convention conflict |
| `finding_1786403096_940640` docs/plans has no mainline placement | process / low | Relocated to `docs/project-pipelines-step-activation-dependency-gate.md` with mainline prior art |

## Scope

### In scope

1. Audit every production entry that can activate an agent step or spawn/link an agent:
   - `start_run`
   - `request_step_advance` (including forced `next_step_id` / `override_unmet_gates`)
   - `activate_step` (including PR review reactivation)
   - `retry_step_agent`
   - command-gate auto-advance (`handle_command_gate_completed` → `request_step_advance`)
   - MCP wrappers and web actions that call those entry points
2. Keep one shared fail-closed helper for open ticket dependencies (prefer one authority: either `blocking_ticket_dependencies` SQL or `unmet_ticket_dependencies`, not divergent filters).
3. Guarantee:
   - no `step.activated` for a target step while open blockers exist
   - no `create_agent` / agent queue / `step.agent_requested` for that target
   - typed response: `ok=false`, `status=blocked`, `reason=ticket_dependencies`, `unmet_dependencies` naming dependency ticket ids
   - `step.advance_blocked` event with source/current/target ids
4. Close residual gaps found by audit (examples if still present):
   - `request_step_advance` must not complete the source visit when target activation is dependency-blocked
   - `request_step_advance` must not return top-level `ok=true` when activation is dependency-blocked
   - last-line defense: refuse spawn inside `spawn_step_agent` if open blockers exist (belt-and-suspenders, still keep primary gate at activation preflight)
5. Expand tests to cover the **live failure sequence**, not only pure unit mocks:
   - mid-run `add_ticket_dependency` while run is past Plan
   - clear review/findings gates
   - advance to Implement refuses
   - retry refuses
   - close dependency, then advance/retry succeeds
   - `start_run` still refuses
6. After code change: install/sync catalog → dogfood plugin path, **reload or restart so the running Hub loads those bytes**, then prove on the live public MCP path with identity pins (see Acceptance).
7. Update plugin README only if the runtime contract changes (keep docs equal to code).
8. After verified ship: vault capture to replace the superseded activation-gating convention with repository-backed current behavior.

### Non-scope

- Playbook-only “agents must check dependencies” instructions without engine enforcement
- Changes to `Projects/botster-project-pipelines` package as dogfood SoT
- Rails / SPA / botster-web client work
- Hub/core spawn primitives (plugin remains policy owner)
- Auto-activation when a dependency closes (explicit advance/retry remains required)
- Plan-loop hygiene / `.gitignore` restore / colon worktree cargo fixes (already merged; do not re-litigate)
- Sibling tickets (`ticket_1786071999_889350` worktree `:`, `ticket_1786072719_543972` resolve_finding)
- Broad pipeline redesign, new primitives, optional configurability, step-level dependency opt-outs

## Repository ownership boundaries and cross-repo dependencies

| Surface | Owner |
| --- | --- |
| Workflow policy: dependency preflight, advance/retry/activate refusal, events, MCP shapes | Project Pipelines monorepo catalog plugin |
| Agent spawn / worktree / session primitives | Hub / core (consumed, not modified) |
| Dogfood runtime install + reload | Operator/plugin install under dogfood plugins path; Hub loads plugin modules |
| Repository engineering conventions for other crates | Out of scope |

Cross-repo dependencies for **this** ticket: none.

Do not register Hub/TUI/kit work as part of this run. Prior incomplete package ticket is historical context only.

## Assumptions and unknowns

### Assumptions

1. Ticket-ordering dependencies in `ticket_dependencies` are the authoritative “blocking dependencies” for this gate (not findings, not questions).
2. “Open” means referenced ticket `status ~= "closed"` (and missing/unavailable tickets fail closed).
3. Final advance with no `next_step` (run completion / merge) is intentionally not a dependency activation gate; README already states this.
4. Closing or removing a dependency never auto-starts work; operator must advance or retry again.
5. Monorepo catalog is the only source of truth for dogfood Project Pipelines after install/sync **and** Hub reload of those modules.

### Unknowns Implement must resolve with evidence

1. Why live run_1786070915 activated Implement despite monorepo PR #200 — stale install vs residual code path.
2. Whether any non-engine caller can create visits or queue agents without `activate_step` / `retry_step_agent`.
3. Whether real SQLite row shapes from `repo.ticket_dependencies` ever omit or mis-alias `depends_on_status` under the live plugin worker.
4. Exact operator step required for the running Hub to load new catalog bytes (plugin hot-reload vs hub restart). Document the step that was used in live proof.

## Affected surfaces / files

Primary:

- `catalog/templates/plugins/project-pipelines/project_pipelines/engine.lua`
- `cli/tests/project_pipelines_plugin_test.rs`
- `catalog/templates/plugins/project-pipelines/README.md` (only if contract text must match a real change)
- Dogfood mirror: `~/.botster-dev/plugins/project-pipelines/**` (install/sync, not a second source tree)

Secondary (touch only if audit finds a gap):

- `catalog/templates/plugins/project-pipelines/project_pipelines/mcp.lua` (error shape / descriptors)
- `catalog/templates/plugins/project-pipelines/project_pipelines/repo.lua` (shared query helper if unified)
- `catalog/templates/plugins/project-pipelines/project_pipelines/web/actions.lua` (only if a UI path bypasses engine)

## Risks

| Risk | Mitigation |
| --- | --- |
| Declare “already fixed” because unit tests pass while dogfood still fails | Require identity-pinned live MCP proof after install/sync + reload |
| Live proof tests a different binary/plugin than the reviewed catalog | Pin source commit, Hub identity, catalog↔installed digests, reload evidence before MCP calls count |
| Complete source visit then fail target activation (stranded done visit) | Dependency preflight before any source completion; do not return top-level success on blocked activation |
| Double filters (`blocking_ticket_dependencies` vs Lua re-filter) diverge | One shared helper |
| Spawn path bypasses activation preflight | Last-line refuse in spawn path + audit all callers |
| Docs claim shipped behavior without runtime proof | README updates only with matching tests + live proof; vault convention only after merge evidence |
| Broad refactor while fixing a preflight | Surgical changes only around dependency preflight and its proofs |

## Acceptance checks / tests

### Unit / catalog (required)

From `cli` with colon-free `CARGO_TARGET_DIR` if needed:

```bash
cd cli
./test.sh -- catalog_plugin_project_pipelines_start_run_blocks_open_dependencies
./test.sh -- catalog_plugin_project_pipelines_public_dependency_write_blocks_advance_before_side_effects
./test.sh -- catalog_plugin_project_pipelines_retry_step_agent_requeues_current_visit
```

Add or extend a fixture that reproduces the **mid-run dependency registration → clear gates → advance Implement** sequence and asserts:

- `ok=false`, `reason=ticket_dependencies`
- zero `create_agent` calls
- zero source visit mutations to `done`
- `step.advance_blocked` event present
- after closing dependency, advance succeeds and spawn is allowed

### Production-path proof (required; identity-pinned)

Live MCP results **do not count** until Implement records all of the following in gate evidence for the same proof session.

#### A. Source and install identity (before MCP calls)

Record exact values:

1. **Refreshed base and source commit**
   - `git rev-parse HEAD` on the implement worktree
   - `git rev-parse origin/main` after `git fetch origin main`
   - Confirm the reviewed catalog files under test are those commits’ content (or the PR branch commit under review)
2. **Running Hub identity**
   - `hub_id` (from `botster context` / hub manifest)
   - Hub process identity available on this device (at least: `pid` from hub manifest, `socket_path`, and `botster --version`)
   - If a stronger build/revision field exists on the running hub, record it too
3. **Catalog ↔ installed digest equality** for every file that carries the gate (minimum `engine.lua`; include `mcp.lua` / `repo.lua` if touched):
   ```bash
   shasum -a 256 \
     catalog/templates/plugins/project-pipelines/project_pipelines/engine.lua \
     "$HOME/.botster-dev/plugins/project-pipelines/project_pipelines/engine.lua"
   ```
   Digests must match. Mismatch means live proof is invalid for the reviewed change.
4. **Reload / restart evidence** after install/sync and **before** the MCP proof calls:
   - State the exact operator action used (plugin hot-reload, hub restart, or documented install path that reloads modules)
   - Record time and any log/event that shows the plugin modules reloaded
   - Without this, digest match alone does not prove the **running worker** loaded those bytes

#### B. Behavioral checks on public MCP tools (after A)

Use public Project Pipelines MCP tools (not private engine requires):

1. `project_pipelines_start_run` with an open blocker still refuses (typed error naming dependency ticket ids).
2. On an active run past Plan, with an open blocker registered mid-flight:
   - `project_pipelines_request_step_advance` into an agent step refuses with `reason=ticket_dependencies`
   - `project_pipelines_retry_step_agent` refuses with `reason=ticket_dependencies`
   - **No target agent request**: no `step.agent_requested` / queued spawn / new agent session for the target step while the dependency is open
   - Record event ids and any session list evidence that proves absence of a new target agent
3. Close or remove the blocker; explicit advance/retry then succeeds; then a target agent may be requested.
4. Cite MCP response bodies + event ids + (when spawn is refused) absence of target session linkage.

#### C. Explicit non-proofs

- README prose without runtime events
- Unit mocks without live install/sync + identity pins
- Live MCP responses without matching digests / Hub identity / reload evidence
- Playbook instructions to agents
- Package-repo changes under `Projects/botster-project-pipelines`

## Implementation sequence (smallest surgical path)

1. Reproduce live sequence against current catalog engine with real-shaped repo fakes, then against installed dogfood plugin.
2. Audit entry points; list every caller of spawn/activate/advance/retry.
3. Close residual fail-open gaps (shared helper, no source-complete-before-target-ok, optional spawn-time assert).
4. Expand tests for mid-run dependency registration.
5. Install/sync monorepo catalog to dogfood plugin path; **reload or restart**; record identity pins (Acceptance A).
6. Run unit suite + identity-pinned live MCP proof (Acceptance B); attach evidence.
7. README only if contract wording must change.
8. Vault inbox capture for a replacement current convention after verified ship.

## Vault gaps worth capturing

1. **Replacement current convention** for step-activation dependency preflight (the prior note remains superseded until new merge+live proof exists).
2. **Dogfood install/sync + Hub reload is part of Project Pipelines delivery** — monorepo catalog changes are not live until installed **and** loaded by the running Hub/worker.
3. **`start_run` gate ≠ active-run activation gate** — both must exist; proving one does not prove the other.
4. **Live proof without artifact identity is not live proof** — Hub id, digests, and reload evidence are required so the proof cannot silently test a different plugin.
5. Optional: **source-complete before target activation is a fail-open footgun** if the second preflight can disagree with the first.

## Product decision ledger

| Item | Decision |
| --- | --- |
| Default | Fail closed on any open/unavailable ticket dependency before agent-step side effects |
| Non-goals | Auto-resume on dependency close; step-level opt-out; package-repo dogfood SoT |
| Follow-up ok | Operator workbench UI polish for blocked-on-dependency state (if not already clear) |
| Ask-human threshold | Only if audit discovers the gate must move into Hub/core primitives rather than plugin policy |

## Plan Review focus (pass 2)

1. Confirm target routing: trybotster monorepo catalog, not package repo, not botster-web.
2. Confirm acceptance A+B close `finding_1786403096_315985` (identity-pinned live proof).
3. Confirm botster-architecture + cli-patterns are recorded with no conflict.
4. Confirm plan placement under tracked `docs/` with mainline prior art.
5. Require Implement to prove live dogfood path with digests and Hub identity, not only unit tests that already existed before the Aug 7 failure.
6. Ensure no silent broadening into package repo or unrelated hygiene tickets.
)
