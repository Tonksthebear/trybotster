# AGENTS.md

## Purpose

Bootstrap every agent session in Botster with the knowledge vault so work follows established conventions instead of rediscovering decisions.

## Required Session Start

Run this first in every session (Claude or Codex):

```bash
bash ~/knowledge/ops/scripts/codex-hooks.sh session-start "$PWD"
```

Treat surfaced conventions as required constraints.

## Source Of Truth

When working in this repo, use these in order:

1. `~/knowledge/CODEX.md` (Codex runtime)
2. `~/knowledge/CLAUDE.md` (vault methodology)
3. `~/knowledge/notes/botster-architecture.md`
4. `~/knowledge/notes/cli-patterns.md`
5. `~/knowledge/notes/rails-conventions.md`
6. local `CLAUDE.md` in this repo

If guidance conflicts, prefer the most specific repo decision, then update vault notes to remove ambiguity.

## How To Use The Vault During Work

- Before major edits: read linked notes for affected area.
- During work: capture new decisions/gotchas to `~/knowledge/inbox/`.
- After work: ask the agent to run document/connect/update/verify phases in the vault.

Capture template:

```markdown
Title: <prose claim>
Why: <mechanism/scope/implication>
Evidence: <file path, behavior, test, or failure mode>
```

## Minimum Conventions To Enforce

- Rails: fat models and POROs over service-object sprawl.
- Frontend: HTML/Tailwind-first patterns; avoid unnecessary JS abstractions.
- Botster architecture: hub is central orchestrator; keep client/server responsibilities explicit.
- Tests: for CLI use `cli/test.sh` (not raw `cargo test`).

## Session End

Run session capture:

```bash
bash ~/knowledge/ops/scripts/codex-hooks.sh session-stop "$(date +%Y%m%d-%H%M%S)" "$PWD"
```

If new durable knowledge was discovered, ensure it is captured in the vault before ending the task.
