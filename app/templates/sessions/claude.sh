#!/bin/bash
# @template Claude
# @description Minimal agent session that launches Claude Code in a worktree
# @category sessions
# @dest shared/sessions/agent/initialization
# @scope device
# @version 1.1.0

# Claude Agent initialization — runs when the agent PTY session starts.
#
# Sets up the worktree environment and launches Claude in acceptEdits mode.
# No MCP server or GitHub integration — just Claude and the codebase.

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

echo "Botster agent initialized for branch: $(botster context branch_name)"
echo "Worktree: $BOTSTER_WORKTREE_PATH"

claude --permission-mode acceptEdits "$(botster context prompt)"
