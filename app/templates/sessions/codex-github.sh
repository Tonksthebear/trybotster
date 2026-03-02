#!/bin/bash
# @template Codex GitHub
# @description Agent session with GitHub MCP tools, issue tracking, and worktree context
# @category sessions
# @dest shared/sessions/agent/initialization
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
# MCP server configuration
# ---------------------------------------------------------------------------
# Build a JSON array of MCP servers for the --mcp-server flag.
# Codex accepts inline JSON: --mcp-server '{"type":"sse","url":"...","headers":{...}}'

MCP_SERVERS=()

# Trybotster remote MCP (GitHub tools, memory, platform capabilities)
if [ -n "$BOTSTER_MCP_TOKEN" ]; then
  MCP_URL="${BOTSTER_MCP_URL:-https://mcp.trybotster.com}"
  echo "Registering trybotster MCP server..."
  MCP_SERVERS+=("$(printf '{"type":"sse","url":"%s","headers":{"Authorization":"Bearer %s"}}' "$MCP_URL" "$BOTSTER_MCP_TOKEN")")
fi

# Botster hub MCP bridge (orchestrator, plugin tools, hub commands)
echo "Registering botster hub MCP tools..."
HUB_SOCKET="$(botster context hub_socket)"
if [ -n "$HUB_SOCKET" ]; then
  MCP_SERVERS+=("$(printf '{"type":"stdio","command":"botster","args":["mcp-serve","--socket","%s"]}' "$HUB_SOCKET")")
else
  MCP_SERVERS+=('{"type":"stdio","command":"botster","args":["mcp-serve"]}')
fi

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

# Build --mcp-server args
MCP_ARGS=()
for server in "${MCP_SERVERS[@]}"; do
  MCP_ARGS+=(--mcp-server "$server")
done

codex --full-auto "${MCP_ARGS[@]}" "$(botster context prompt)"
