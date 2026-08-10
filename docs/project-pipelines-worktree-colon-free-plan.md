# Plan: Worktree directory names must not contain `:` (Cargo / macOS DYLD)

**Ticket:** `ticket_1786071999_889350`  
**Run:** `run_1786401761_237076`  
**Pipeline step:** Plan (`botster_stack_plan`) — revision 2 after Plan Review `changes_required`  
**Date:** 2026-08-10  

## Plan Review findings addressed (this revision)

| Finding | Class | Correction in this plan |
| --- | --- | --- |
| `finding_1786402625_598926` — plugin verification for engine change | product / high | Conditional charter gate: any `engine.lua` or plugin catalog change requires `project_pipelines_plugin_test`, catalog→install sync, plugin reload, and real Hub / public tool proof with commit + clean-tree pins |
| `finding_1786402625_735236` — record CLI + architecture context | product / low | Loaded [[cli-patterns]] and [[botster-architecture]]; listed in notes ledger; impact on assumptions/acceptance recorded below |

## Target repository and target_id

| Field | Value |
| --- | --- |
| **target_id** | `tgt_83619444571645afa5507374e36036e2` |
| **target_repository** | trybotster (monorepo) |
| **Spawn target name** | trybotster |
| **Spawn path** | resolved by hub (authoritative target; not ambient cwd) |
| **Ticket routing** | Monorepo built-in plugin only: `catalog/templates/plugins/project-pipelines/` |
| **Install/sync path** | `~/.botster-dev/plugins/project-pipelines/` |

There is no separate “trybotster” charter in the repository routing map. This ticket owns **Project Pipelines package/plugin paths** in the monorepo, so the repository ownership charter is [[project-pipelines-playbook]]. Directory naming for agent worktrees is implemented in monorepo **CLI** (`cli/src/git.rs`), which already landed on `main` with this ticket’s hygiene parent work.

## Repository playbook loaded

- [[project-pipelines-playbook]] — ownership charter for Project Pipelines engine, worktrees, gates, artifacts, MCP

## Other role / surface playbooks and atomic notes loaded

### Role / stack
- [[planner-playbook]]
- [[botster-planner-playbook]]

### Required Botster context (must-load from botster-planner-playbook)
- [[cli-patterns]] — Rust CLI / hub / worktree / test-wrapper patterns; product policy stays in plugins; use `cli/test.sh` not raw `cargo test`
- [[botster-architecture]] — platform map and ownership charters; [[project-pipelines-playbook]] is the plugin workflow charter; CLI owns worktree mechanisms

### Targeted atomic notes
- [[colon worktree paths break cargo dyld library paths]]
- [[project pipeline step activation must preserve tracked gitignore]]
- [[hearth gate runs require restoring a pipeline wiped gitignore before attribution]]
- [[plan agents must author vault context as wikilinks not home paths]]
- [[pipeline vault checklists must cite exact resolvable note titles]]
- [[vault example paths are not repository placement conventions]]
- [[project pipeline orchestration belongs in a device-level botster plugin]]
- [[botster orchestration prompts must bind agents to explicit worktrees]]
- [[plan review must verify a plan artifact exists before trusting gate summaries]]
- [[test script required for rust tests not cargo test]]

### Explicitly not loaded
- [[botster runtime teardown lenses]] — not applicable (no WebRTC / SessionIo / multi-peer teardown class)
- Repository charters for botster-core / hub / web / tui / etc. as **implementation targets** — not this ticket’s target_id (architecture map names them; this run does not broaden delivery to those repos)
- Separate package repo `botster-project-pipelines` — ticket forbids dogfood SoT there

### Impact of loading cli-patterns + botster-architecture
- **Assumptions unchanged:** monorepo CLI owns worktree path components; Project Pipelines owns workflow policy and command-gate env; product policy does not move into core.
- **Acceptance tightened:** Rust gates always use `cd cli && ./test.sh …` (BOTSTER_ENV=test). Plugin engine edits additionally require the charter plugin suite below.
- **No new scope:** package-boundary notes confirm not to implement this fix in the separate package repo.

## Context loaded

### Problem (from ticket + vault)
On macOS, a worktree directory that contains `:` (for example SSH-style `git@github.com:…` embedded in the path) can make Cargo fail before compile when it injects `target/…/deps` into `DYLD_FALLBACK_LIBRARY_PATH`. Dyld path lists use `:` as the separator, so a path segment that already contains `:` is invalid.

Observed failure class: `path segment contains separator ':'` during `script/test` / `script/clippy` in a colon-containing pipeline worktree. Base is green when `CARGO_TARGET_DIR` is forced to a colon-free directory. `script/test-live-hub` already uses `mktemp` for `CARGO_TARGET_DIR` and is unaffected.

### Already on monorepo `main` (audit result — do not re-implement)

| Commit | Change |
| --- | --- |
| `66fd54df` | `cli/src/git.rs` + `cli/src/agent/mod.rs`: `path_component_safe` strips `:`, `@`, path separators, and other path-hostile characters from worktree / clone directory components |
| `d72ad2da` (PR #207) | Project Pipelines: gitignore restore-on-empty, command-gate `CARGO_TARGET_DIR` when cwd contains `:`, stack delivery prompt refresh, README hygiene section |

Unit coverage already present:
- `path_component_safe_strips_colon_and_at` in `cli/src/git.rs`

Plugin engine already present:
- `cargo_target_dir_for(cwd)` → colon-free dir under `$TMPDIR/botster-cargo-target/…` only when cwd contains `:`
- Applied on **command steps** via `run_command_step` env injection
- `ensure_worktree_hygiene` / gitignore restore on activation and agent link

### Live path naming simulation (Plan-time)

For SSH remote `git@github.com:trybotster/botster-tui.git`:
- `repo_name_for_root` currently yields intermediate `git@github.com:trybotster/botster-tui` (SSH host colon survives `/`-split parsing)
- `path_component_safe` yields `git-github.com-trybotster-botster-tui` — **no `:`**

For HTTPS `https://github.com/Tonksthebear/trybotster.git`:
- yields `Tonksthebear-trybotster` — never had a colon even before sanitize

**Important:** This Plan worktree path (`Tonksthebear-trybotster-project-pipelines-ticket_…`) is HTTPS-derived and therefore **does not prove** the SSH / colon case by itself.

Legacy sessions under the botster sessions base still include names with `git@github.com:…` (pre-sanitize hub). New worktrees must not recreate that shape once the running hub binary includes `66fd54df`.

### Related open siblings (same monorepo target; separate scope)
- `ticket_1786072718_293091` — step-activation dependency gate
- `ticket_1786072719_543972` — `resolve_finding` not-found

Do not fold those into this plan.

## Scope

**Primary intent:** Pipeline worktree / session directory names avoid `:` (and other path-hostile characters), and Rust script/test gates succeed without the agent manually setting `CARGO_TARGET_DIR`.

Implement must **audit `main` first**, then **close residual gaps only**.

### In scope
1. Confirm production entry points for new agent worktrees use `path_component_safe` (create/reuse paths in monorepo CLI `WorktreeManager` + agent id naming).
2. Close any remaining monorepo gaps that still emit `:` into worktree directory components on current `main` (only if audit finds one).
3. Keep / tighten Project Pipelines defense-in-depth:
   - command-gate colon-free `CARGO_TARGET_DIR` when path still contains `:` (legacy reuse)
   - optional residual: inject the same env on agent spawn when the resolved worktree path contains `:` (agents run Cargo from the PTY; command-step env alone does not cover agent-driven `script/test`)
4. Strengthen proof tests if gaps are closed or if coverage is unit-only for the string helper:
   - keep existing `path_component_safe` unit test
   - prefer an end-to-end assertion that a worktree path built from an SSH-style repo identity contains no `:`
5. Dual-write catalog → `~/.botster-dev/plugins/project-pipelines/` after any plugin change; reload plugin.
6. Live production-path proof (required for acceptance — see below).

### Non-scope
- Reworking unrelated Project Pipelines features (dependency activation, resolve_finding, UI, SPA)
- Editing the separate `botster-project-pipelines` package as dogfood source of truth
- Migrating or deleting legacy colon worktrees on disk (document residual; do not bulk delete in this ticket)
- Broad SSH remote-url beauty refactor unless needed to close a real `:` emission bug
- botster-core / botster-hub **package** repos as delivery targets (this ticket is monorepo trybotster)
- Runtime-teardown class work
- Hub session-type eligibility consumer pins (not applicable)

## Repository ownership boundaries and cross-repo dependencies

| Layer | Owner | Status on main |
| --- | --- | --- |
| Worktree directory component sanitization | Monorepo CLI (`cli/src/git.rs`, agent naming) | Landed `66fd54df` |
| Command-gate `CARGO_TARGET_DIR` + gitignore hygiene | Project Pipelines plugin (`catalog/templates/plugins/project-pipelines/`) | Landed `d72ad2da` |
| Hub spawn primitives that call WorktreeManager | Monorepo hub binary / session path | Must run a binary built from post-`66fd54df` tree |
| Package repo `botster-project-pipelines` | Out of scope for dogfood SoT | Do not edit for this fix |
| botster-core package tree | Not this target | If a non-monorepo hub is ever the production binary, register a **separate** core/hub ticket; do not silently broaden this run |

**Cross-repo dependencies to register:** none for the monorepo path, provided dogfood hub runs monorepo CLI with the sanitize commit.

**Ops residual (not a new product ticket by default):** dogfood hub must be the monorepo build that includes `path_component_safe`. A stale `/usr/local/bin/botster` release from before the fix is not sufficient proof.

## Assumptions and unknowns

### Assumptions
1. Ticket target_id correctly binds trybotster monorepo; implement/verify stay there.
2. Production path for pipeline agent worktrees is monorepo `WorktreeManager` + hub create_agent (not a parallel plugin-local path builder).
3. PR #207 residual human steps (reload plugin, hub binary with sanitize) may still be incomplete on some machines; this run must prove the running path, not only git history.
4. Prefer permanent colon-free names over relying forever on `CARGO_TARGET_DIR` workarounds; keep the env injection as defense-in-depth for legacy paths.

### Unknowns to resolve in Implement / Verify
1. Whether the **currently running** dogfood hub process was built after `66fd54df` (code on disk ≠ binary in memory).
2. Whether any alternate worktree naming path still concatenates raw remote strings without `path_component_safe`.
3. Whether agent spawn should inject `CARGO_TARGET_DIR` when reusing a pre-existing colon worktree via `from_worktree` (likely yes for residual risk; only if production still reuses such paths).
4. Whether SSH `repo_name_for_root` should normalize to `owner/repo` for cleaner names (optional polish; not required if sanitize always applies before join).

## Affected surfaces / files

### Likely touch (only if residual gaps)
- `cli/src/git.rs` — `path_component_safe`, `create_worktree*`, optional SSH name normalize, tests
- `cli/src/agent/mod.rs` — agent_id naming (already uses sanitize)
- `catalog/templates/plugins/project-pipelines/project_pipelines/engine.lua` — `cargo_target_dir_for`, optional agent-spawn env, hygiene hooks
- `catalog/templates/plugins/project-pipelines/README.md` — worktree hygiene contract (already documents colon-free `CARGO_TARGET_DIR`)
- Install dual-write: `~/.botster-dev/plugins/project-pipelines/`

### Plan artifact placement
- This file: `docs/project-pipelines-worktree-colon-free-plan.md` (alongside existing `docs/project-pipelines-verification.md` prior art; not a vault-example `docs/plans/` path)

## Risks

1. **False green from HTTPS worktrees** — trybotster’s remote is HTTPS, so new worktrees look colon-free without proving the SSH failure mode. Proof must use an SSH-remote target (for example botster-tui).
2. **Stale hub binary** — code on `main` with a hub process built earlier still creates colon paths.
3. **Legacy worktree reuse** — `from_worktree` can reattach to old `git@github.com:…` directories; path sanitize on create does not rewrite history.
4. **Command-step-only env** — agent-driven Cargo still fails on colon paths unless names are fixed or agent env is injected.
5. **RTK / summarized cargo output** can hide the dyld separator error — keep raw logs for gate evidence ([[botster pipeline reviewers must bypass rtk summaries for cargo gate evidence]] if used).
6. **Scope creep** into package repos or sibling PP tickets.

## Acceptance checks / tests

### Static / unit (CLI naming)
1. `cd cli && ./test.sh --unit path_component_safe` (or equivalent filter) — must pass. Never use raw `cargo test` for monorepo CLI ([[cli-patterns]] / monorepo contract).
2. Grep production create paths: every worktree directory join that includes repo/branch identity must go through `path_component_safe` (no raw `format!("{}-{}", repo, branch)` with unsanitized repo).
3. Plugin engine still contains `cargo_target_dir_for` and applies it on command gates when path contains `:`.

### Conditional charter gate — any Project Pipelines engine / plugin change
**Trigger:** Implement edits any of:
- `catalog/templates/plugins/project-pipelines/project_pipelines/engine.lua`
- other files under `catalog/templates/plugins/project-pipelines/` that change runtime behavior (not plan-only docs outside the plugin tree)

**Required when triggered (from [[project-pipelines-playbook]] Required Gates):**
1. Record commit SHA and clean tracked state **before** the gate.
2. Run plugin unit/schema coverage through the monorepo wrapper:
   - `cd cli && ./test.sh -- project_pipelines_plugin_test`
3. Sync catalog to the dogfood install path:
   - `rsync -a --delete catalog/templates/plugins/project-pipelines/ ~/.botster-dev/plugins/project-pipelines/`
4. Reload the project-pipelines plugin on the real hub.
5. Prove the changed behavior through the **real Hub / plugin-worker path and public Project Pipelines tools** (not handler-only calls). Minimum for this ticket when engine env or hygiene changes:
   - exercise a path that invokes the changed code (command gate with colon path and/or agent spawn hygiene as applicable)
   - show durable evidence (event payload, gate result, or MCP tool response) that colon-free `CARGO_TARGET_DIR` or hygiene ran as designed
6. Record commit SHA and clean tracked state **after** the gate; discard and rerun if either changed mid-gate.

**When not triggered:** zero-delta / CLI-only residual proof may skip the plugin suite **only if** no catalog plugin file changed. The plan still requires live SSH worktree proof for naming.

### Production-path / live proof (required always)
1. Using a spawn target whose `origin` is SSH-style (`git@github.com:…`), create a **new** pipeline/agent worktree (not a reused legacy path).
2. Assert the worktree absolute path basename/components contain **no** `:` (and preferably no `@`).
3. From that worktree, run the target’s Rust test entrypoint **without** the agent manually exporting `CARGO_TARGET_DIR` (for monorepo CLI: `cd cli && ./test.sh` subset or project `script/test` as applicable). Expect green join of DYLD paths / no separator error.
4. If Implement only hardens plugin env and does not rebuild hub naming, still prove new worktree names under the running hub — code existence alone is insufficient.

### Install / dogfood
1. After plugin edits: rsync catalog → `~/.botster-dev/plugins/project-pipelines/` and reload project-pipelines (same as conditional charter gate steps 3–4).
2. Confirm running hub includes sanitize (rebuild/restart monorepo hub if needed).

### Explicitly not sufficient
- HTTPS-only worktree path without `:`
- Unit test of the string helper alone without live create path
- Manual `CARGO_TARGET_DIR` from the agent as the permanent success path
- Plugin `engine.lua` change with only “grep for cargo_target_dir_for” or sync/reload without `project_pipelines_plugin_test` and public-tool proof

## Runtime-teardown class

**Does not apply.** No teardown lens fields required.

## Implement order (smallest surgical path)

1. Re-audit `main` worktree naming call sites; list any unsanitized join.
2. If none: treat product fix as landed; add only missing proof tests + live proof; dual-write/reload only if plugin docs or env residual change.
3. If residual emission of `:` exists: fix at CLI naming (preferred permanent fix).
4. If legacy reuse still hits agent Cargo: inject colon-free `CARGO_TARGET_DIR` on agent spawn when path contains `:` (plugin residual, defense-in-depth).
5. **If any plugin/engine file changed:** run the full conditional charter gate (plugin test suite → sync → reload → public-tool proof with commit/clean pins).
6. Sync plugin install path; rebuild/restart hub if binary is stale.
7. Capture live SSH-remote worktree path + cargo gate evidence in Implement artifacts.

## Vault gaps worth capturing

1. **Dogfood hub binary age vs monorepo `main`** — operational gotcha: sanitize on disk does not help until hub restarts on that binary.
2. **HTTPS worktrees are not proof of the SSH colon class** — verification must force SSH-style remote identity.
3. Optional: **SSH remote parse in `repo_name_for_root` leaves intermediate `git@host:owner/repo`** — currently safe only because sanitize runs after; worth a short note if not already fully covered by [[colon worktree paths break cargo dyld library paths]].

## Product decision ledger (compact)

| Item | Decision |
| --- | --- |
| Default fix | Colon-free worktree directory components at create time |
| Defense-in-depth | Colon-free `CARGO_TARGET_DIR` when path still contains `:` |
| Package repo edits | Out of scope |
| Zero-delta OK? | Yes if audit finds no residual emission **and** live SSH proof is green under current hub |
| Ask human if | Running hub cannot be rebuilt/restarted for proof, or target_id routing is disputed |

## Gate evidence anchors for Plan completion

- `target_id`: `tgt_83619444571645afa5507374e36036e2`
- `target_repository`: trybotster
- `repository_playbook`: project-pipelines-playbook
- `plan_uri`: `docs/project-pipelines-worktree-colon-free-plan.md` (this file in the run worktree)
- `checklist_id`: filled by Plan agent vault checklist for this run
- `teardown_class_applies`: false
