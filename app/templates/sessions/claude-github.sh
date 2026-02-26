#!/bin/bash
# @template Claude GitHub
# @description Agent session with GitHub MCP tools, issue tracking, and worktree context
# @category sessions
# @dest shared/sessions/agent/initialization
# @scope device
# @version 1.1.0

# Claude Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment, registers MCP tools, and launches
# Claude in acceptEdits mode with the task prompt.

# Uncomment if using mise
# mise trust 2>/dev/null

# Change to the worktree directory
cd "$BOTSTER_WORKTREE_PATH"

# ---------------------------------------------------------------------------
# Claude trust
# ---------------------------------------------------------------------------
# Auto-accept the trust dialog for this worktree so Claude doesn't prompt.

botster json-set ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH.hasTrustDialogAccepted" "true"

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
# Hub MCP tools
# ---------------------------------------------------------------------------
# Registers the local hub MCP bridge so agents can use plugin-provided tools
# (orchestrator, custom plugin tools, etc.) via the Botster hub.
# The mcp-serve subcommand auto-discovers the hub socket from the cwd.

echo "Registering botster hub MCP tools..."
claude mcp add botster-hub -- botster mcp-serve

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------

ISSUE=$(botster context issue_number)
if [ "$ISSUE" != "0" ] && [ -n "$ISSUE" ]; then
  echo "Botster agent initialized for issue #$ISSUE"
else
  echo "Botster agent initialized for branch: $(botster context branch_name)"
fi
echo "Worktree: $BOTSTER_WORKTREE_PATH"

claude --permission-mode acceptEdits "$(botster context prompt)"
