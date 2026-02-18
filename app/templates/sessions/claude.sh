#!/bin/bash
# @template Claude
# @description Agent session that launches Claude Code with MCP tools and worktree context
# @category sessions
# @dest shared/sessions/agent/initialization
# @scope device
# @version 1.0.0

# Claude Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment, registers MCP tools, and launches
# Claude in acceptEdits mode with the task prompt.

# Trust mise config in the worktree
mise trust 2>/dev/null

# Change to the worktree directory
cd "$BOTSTER_WORKTREE_PATH"

# ---------------------------------------------------------------------------
# Context resolution
# ---------------------------------------------------------------------------
# Worktrees get a .botster/context.json with full task metadata.
# Manual agents on main fall back to environment variables.

botster_context() {
  local val
  val=$("$BOTSTER_BIN" json-get .botster/context.json "$1" 2>/dev/null | tr -d '"')
  echo "$val"
}

BOTSTER_PROMPT=$(botster_context "prompt")
BOTSTER_PROMPT="${BOTSTER_PROMPT:-$BOTSTER_TASK_DESCRIPTION}"

BOTSTER_ISSUE_NUMBER=$(botster_context "issue_number")
BOTSTER_ISSUE_NUMBER="${BOTSTER_ISSUE_NUMBER:-$BOTSTER_ISSUE_NUMBER}"

BOTSTER_BRANCH_NAME=$(botster_context "branch_name")
BOTSTER_BRANCH_NAME="${BOTSTER_BRANCH_NAME:-$BOTSTER_BRANCH_NAME}"

# ---------------------------------------------------------------------------
# Claude trust
# ---------------------------------------------------------------------------
# Auto-accept the trust dialog for this worktree so Claude doesn't prompt.

"$BOTSTER_BIN" json-set ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH.hasTrustDialogAccepted" "true"

# ---------------------------------------------------------------------------
# MCP server registration
# ---------------------------------------------------------------------------
# Registers the trybotster MCP server so agents can use GitHub tools,
# memory, and other platform capabilities.

if [ -n "$BOTSTER_MCP_TOKEN" ]; then
  MCP_URL="${BOTSTER_MCP_URL:-https://mcp.trybotster.com}"
  echo "Registering trybotster MCP server..."
  claude mcp add trybotster \
    --transport http \
    "$MCP_URL" \
    --header \
    "Authorization: Bearer $(echo "$BOTSTER_MCP_TOKEN")"
fi

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------

if [ "$BOTSTER_ISSUE_NUMBER" != "0" ] && [ -n "$BOTSTER_ISSUE_NUMBER" ]; then
  echo "Botster agent initialized for issue #$BOTSTER_ISSUE_NUMBER"
else
  echo "Botster agent initialized for branch: $BOTSTER_BRANCH_NAME"
fi
echo "Worktree: $BOTSTER_WORKTREE_PATH"

claude --permission-mode acceptEdits "$BOTSTER_PROMPT"
