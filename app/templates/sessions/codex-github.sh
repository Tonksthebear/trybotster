#!/bin/bash
# @template Codex GitHub
# @description Agent session with GitHub MCP tools, issue tracking, and worktree context
# @category sessions
# @dest agents/codex/initialization
# @scope device
# @version 1.0.0

# Codex Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment, registers MCP tools, and launches
# Codex in full-auto mode with the task prompt.

# Uncomment if using mise
# mise trust 2>/dev/null

# Change to the worktree directory
cd "$BOTSTER_WORKTREE_PATH"

# ---------------------------------------------------------------------------
# MCP server registration
# ---------------------------------------------------------------------------
# Codex MCP servers are registered via `codex mcp add`.
# The old `--mcp-server` flag is no longer supported by Codex CLI.

# Trybotster remote MCP (GitHub tools, memory, platform capabilities)
if [ -n "$BOTSTER_MCP_TOKEN" ]; then
  MCP_URL="${BOTSTER_MCP_URL:-https://mcp.trybotster.com}"
  echo "Registering trybotster MCP server..."
  codex mcp remove trybotster >/dev/null 2>&1 || true
  codex mcp add trybotster --url "$MCP_URL" --bearer-token-env-var BOTSTER_MCP_TOKEN
fi

# Botster hub MCP bridge (orchestrator, plugin tools, hub commands)
echo "Registering botster hub MCP tools..."
codex mcp remove botster-hub >/dev/null 2>&1 || true
codex mcp add botster-hub -- botster mcp-serve

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

codex --full-auto "$(botster context prompt)"
