#!/bin/bash
# @template Claude
# @description Minimal agent session that launches Claude Code in a worktree
# @category sessions
# @dest shared/sessions/agent/initialization
# @scope device
# @version 1.0.0

# Claude Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment and launches Claude in acceptEdits mode.
# No MCP server or GitHub integration — just Claude and the codebase.

# Uncomment if using mise
# mise trust 2>/dev/null

# Change to the worktree directory
cd "$BOTSTER_WORKTREE_PATH"

# ---------------------------------------------------------------------------
# Context resolution
# ---------------------------------------------------------------------------
# Worktrees get a .botster/context.json with full task metadata.
# Manual agents on main fall back to environment variables.

botster_context() {
  local val
  val=$(botster json-get .botster/context.json "$1" 2>/dev/null | tr -d '"')
  echo "$val"
}

BOTSTER_PROMPT=$(botster_context "prompt")
BOTSTER_PROMPT="${BOTSTER_PROMPT:-$BOTSTER_TASK_DESCRIPTION}"

BOTSTER_BRANCH_NAME=$(botster_context "branch_name")
BOTSTER_BRANCH_NAME="${BOTSTER_BRANCH_NAME:-$BOTSTER_BRANCH_NAME}"

# ---------------------------------------------------------------------------
# Claude trust
# ---------------------------------------------------------------------------
# Auto-accept the trust dialog for this worktree so Claude doesn't prompt.

botster json-set ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH.hasTrustDialogAccepted" "true"

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------

echo "Botster agent initialized for branch: $BOTSTER_BRANCH_NAME"
echo "Worktree: $BOTSTER_WORKTREE_PATH"

claude --permission-mode acceptEdits "$BOTSTER_PROMPT"
