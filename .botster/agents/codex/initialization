#!/bin/bash
# @template Codex
# @description Minimal agent session that launches OpenAI Codex in a worktree
# @category sessions
# @dest profiles/codex/sessions/agent/initialization
# @scope device
# @version 1.0.0

# Codex Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment and launches Codex in full-auto mode.
# No MCP server or GitHub integration — just Codex and the codebase.

# Uncomment if using mise
# mise trust 2>/dev/null

# Change to the worktree directory
cd "$BOTSTER_WORKTREE_PATH"

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------

echo "Botster agent initialized for branch: $(botster context branch_name)"
echo "Worktree: $BOTSTER_WORKTREE_PATH"

codex --full-auto "$(botster context prompt)"
