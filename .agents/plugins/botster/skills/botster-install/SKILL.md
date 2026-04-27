---
name: botster-install
description: Use when installing Botster for an agent, setting up the first Botster-managed agent, or checking whether Botster MCP and agent configuration are present.
---

# Botster Install

Use this skill to make a fresh agent usable inside Botster. The goal is a
single Botster MCP server, one working agent definition, and the core Botster
plugins needed for coordination.

## Expected Agent Plugin State

The agent plugin should provide this MCP server automatically:

```toml
[mcp_servers.botster]
command = "botster"
args = ["mcp-serve"]
env_vars = ["BOTSTER_SESSION_UUID"]
```

Do not add duplicate Botster MCP aliases. `botster mcp-serve` resolves caller
identity from `BOTSTER_SESSION_UUID`; duplicate aliases make tool discovery
noisy without adding capability.

## Expected Botster Runtime State

For a first useful agent setup, verify these exist:

- An admitted spawn target for the repo the user wants agents to work in.
- An agent definition under the active Botster config directory, usually
  `agents/<name>/initialization`.
- The agent initialization changes to `botster context worktree_path`.
- The initialization launches the agent CLI with the task prompt from
  `botster context prompt`.
- The spawned agent process inherits `BOTSTER_SESSION_UUID`.
- Coordination plugins such as `orchestrator` and `messaging` are available if
  the user wants multi-agent workflows.

## First-Agent Checklist

1. Confirm the Botster CLI is installed and on `PATH`.
2. Confirm the agent plugin installed the `botster` MCP server.
3. Confirm the repo is admitted as a Botster spawn target.
4. Create or select an agent profile/definition.
5. Start one agent and call `whoami` through MCP.
6. If `whoami` fails, check `BOTSTER_SESSION_UUID` propagation before changing
   tools or plugin code.

## Boundaries

Botster is the hub and PTY orchestrator. Agent plugins configure agent-side
integration; Botster Lua plugins provide runtime MCP tools. Keep those layers
separate even when the user experience is one plugin install.
