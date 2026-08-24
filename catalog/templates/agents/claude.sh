#!/bin/bash
# @template Claude
# @description Launch Claude Code with the session prompt only
# @category agents
# @dest agents/claude/initialization
# @scope device
# @version 1.2.0

# Claude Agent initialization — runs when the agent PTY session starts.
#
# Botster already sets the PTY cwd to the worktree. Do not mutate Claude
# config, register MCP servers, rewrite project trust, or otherwise edit
# shared Claude/workspace state from this script.

claude "$(botster context prompt)"
