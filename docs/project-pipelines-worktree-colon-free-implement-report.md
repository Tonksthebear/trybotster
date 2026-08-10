# Implement report: colon-free worktree directory names

**Ticket:** `ticket_1786071999_889350`  
**Run:** `run_1786401761_237076`  
**Step:** Implement (`botster_stack_implement`)  
**Date:** 2026-08-10  

## Target repository and target_id

| Field | Value |
| --- | --- |
| **target_id** | `tgt_83619444571645afa5507374e36036e2` |
| **target_repository** | trybotster (monorepo) |
| **Routing** | Matches approved plan revision 2 |
| **Repository playbook** | [[project-pipelines-playbook]] |
| **Worktree** | run worktree for this ticket only |

## Repository playbook and other playbooks/notes applied

### Ownership charter
- [[project-pipelines-playbook]]

### Role / stack
- [[implementer-playbook]]
- [[botster-implementer-playbook]]

### Required Botster context
- [[cli-patterns]] — `cli/test.sh` wrapper; product policy stays in plugins
- [[botster-architecture]] — CLI owns worktree mechanisms; Project Pipelines owns workflow policy

### Targeted notes
- [[colon worktree paths break cargo dyld library paths]]
- [[test script required for rust tests not cargo test]]
- [[implement gate must verify committed work and pr link before review]]
- [[implementation steps must persist report artifacts for review]]
- [[implementation artifacts must match actual git state]]
- [[pipeline vault checklists must cite exact resolvable note titles]]

### Explicitly not loaded / not applicable
- [[botster runtime teardown lenses]] — plan `teardown_class_applies: false`
- botster-core / hub / web / tui package charters as delivery targets
- separate `botster-project-pipelines` package repo (out of scope)

## Audit of main (product fix already landed)

| Layer | Status on `4a8a74ee` / `origin/main` |
| --- | --- |
| `path_component_safe` in `cli/src/git.rs` | Present (`66fd54df` ancestor) |
| Worktree create paths (`create_worktree_for_repo_root_from_ref`, `create_worktree`) | Join only sanitized repo + branch/issue components |
| Agent id naming (`cli/src/agent/mod.rs`) | Uses `path_component_safe` |
| Hub production entry | `event_lua` → `WorktreeManager::create_worktree_for_repo_root_from_ref` |
| Plugin command-gate `CARGO_TARGET_DIR` | Present in `engine.lua` (`cargo_target_dir_for`) for cwd containing `:` (`d72ad2da` ancestor) |

**Residual emission audit:** no remaining production create path joins raw remote strings into directory components without `path_component_safe`.

## Files changed

| Path | Change |
| --- | --- |
| `cli/src/git.rs` | Added unit test `create_worktree_for_ssh_remote_path_has_no_colon` (temp repo + SSH origin → create path has no `:`/`@`) |
| `cli/tests/ssh_worktree_live_proof.rs` | Added integration test `worktree_manager_ssh_origin_path_has_no_colon` (public API E2E) |
| `docs/project-pipelines-worktree-colon-free-plan.md` | Commit approved plan revision 2 into the run branch |
| `docs/project-pipelines-worktree-colon-free-implement-report.md` | This report |

**No plugin/engine runtime edits.** Conditional Project Pipelines plugin suite gate was not triggered.

## Ownership boundaries preserved

- Worktree path component sanitization remains monorepo CLI (`cli/src/git.rs` / agent naming).
- Command-gate colon-free `CARGO_TARGET_DIR` remains Project Pipelines plugin policy (already on main).
- No edits to package repo `botster-project-pipelines`.
- No broadening to botster-core / botster-hub package trees.

## Cross-repo dependencies or separately routed work

None for monorepo delivery. Related open tickets (`ticket_1786072718_293091`, `ticket_1786072719_543972`) keep separate scope.

## Deviations from plan

| Plan item | Outcome |
| --- | --- |
| Close residual `:` emission if found | **None found** — product fix treated as landed |
| Optional agent-spawn `CARGO_TARGET_DIR` for legacy colon paths | **Not implemented** — `Hub:create_agent` has no env map; would require hub agent-spawn API expansion beyond surgical plugin residual |
| Plugin dual-write / reload | **Skipped** — no plugin file runtime change |
| Live SSH worktree + cargo without manual `CARGO_TARGET_DIR` | **Done** (see proof) |

No accepted product scope change that requires plan re-sync of acceptance checks; residual is documented.

## Tests and downstream proof run

### Unit / integration (repository wrapper)

```text
cd cli && ./test.sh --unit path_component_safe
# test git::tests::path_component_safe_strips_colon_and_at ... ok

cd cli && ./test.sh --unit create_worktree_for_ssh_remote
# test git::tests::create_worktree_for_ssh_remote_path_has_no_colon ... ok

cd cli && BOTSTER_ENV=test cargo test --test ssh_worktree_live_proof
# test worktree_manager_ssh_origin_path_has_no_colon ... ok
```

(`cargo test --test` used only for the single integration binary after `BOTSTER_ENV=test`; unit suite used `./test.sh`.)

### Live SSH-origin create (WorktreeManager production API)

Against real checkout `/Users/jasonconigliari/Projects/botster-tui` with  
`origin = git@github.com:trybotster/botster-tui.git`:

```text
LIVE_WORKTREE_PATH=/tmp/botster-colon-mgr.FjhHqN/git-github.com-trybotster-botster-tui-project-pipelines-colon-cargo-proof-1786403424
ORIGIN_REMOTE=git@github.com:trybotster/botster-tui.git
```

Assertions: path contains **no** `:` and **no** `@`.

### Cargo gate on that path without agent-set `CARGO_TARGET_DIR`

From the colon-free worktree, with `CARGO_TARGET_DIR` unset:

```text
cargo metadata --no-deps --format-version 1   # ok
cargo test --no-run                             # Finished test profile in ~21s
```

No `path segment contains separator ':'` / DYLD join failure.  
Log: `/tmp/colon-cargo-test-norun.log` (Finished `test` profile; executables emitted).

### Dogfood hub binary

- Running dogfood hub: `/Users/jasonconigliari/Rails/trybotster/cli/target/debug/botster start --headless` (mtime 2026-08-10 15:19).
- Monorepo tree includes sanitize commit `66fd54df`.
- This ticket’s run worktree is HTTPS-named (`Tonksthebear-trybotster-…`) and does not itself prove SSH; live proof used botster-tui SSH origin intentionally.
- Legacy on-disk sessions still include `git@github.com:…` names (pre-sanitize); not bulk-deleted per plan.

### Plugin charter suite

Not run — no `catalog/templates/plugins/project-pipelines/**` runtime edits.

## Unverified behavior or residual risk

1. **Legacy colon worktrees** still exist under `botster-sessions/` (e.g. botster-tui ticket `1785612604`). Create-path sanitize does not rewrite history. Command gates get `CARGO_TARGET_DIR`; agent PTY cargo may still fail on those paths without manual env.
2. **Agent-spawn env injection** not wired (`create_agent` has no env field). Permanent fix remains colon-free names; env is defense-in-depth only for reuse of legacy paths.
3. **Hub MCP `list_spawn_targets` timed out** during Implement — live create proof used `WorktreeManager` (same code path as hub `worktree.create`), not a full pipeline agent spawn through public Project Pipelines tools.
4. **Stale `/usr/local/bin/botster` (May 5)** is not the dogfood hub binary; operators must use monorepo-built hub for naming fix.

## Missing vault guidance discovered

1. Dogfood hub binary age vs monorepo `main` — operational gotcha (plan already listed).
2. HTTPS worktrees are not SSH colon-class proof (plan already listed).
3. Optional: intermediate `repo_name_for_root` still yields `git@host:owner/repo` before sanitize — safe only because sanitize always runs before join; worth capturing if not fully covered by [[colon worktree paths break cargo dyld library paths]].

## Production entry point for new behavior

Hub async worktree create (`cli/src/hub/server_comms/event_lua.rs`) calls  
`WorktreeManager::create_worktree_for_repo_root_from_ref`, which always sanitizes repo + branch path components.  
New agent/pipeline worktrees therefore cannot embed `:` from SSH remotes once the hub binary includes `66fd54df`+.

## Runtime-teardown class

Does not apply.

## Commit / PR

| Field | Value |
| --- | --- |
| **Branch** | `project-pipelines/ticket_1786071999_889350` |
| **Commit** | `ca9e3253` (plus any report-link follow-up) |
| **PR** | https://github.com/Tonksthebear/trybotster/pull/208 |
