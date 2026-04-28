# Botster Agent Plugin

This package is the agent-side Botster integration. It installs the Botster MCP
server configuration and ships Botster workflow skills so agents do not need
manual MCP setup or generic default MCP prompt discovery.

Botster Lua plugins still own the runtime tool surface. This package only
connects the agent to `botster mcp-serve` and teaches the agent how to use the
hub, session, messaging, and orchestration tools correctly.

## Included

- `.codex-plugin/plugin.json` — plugin manifest.
- `.claude-plugin/plugin.json` — Claude Code plugin manifest.
- `.mcp.json` — single MCP server named `botster`.
- `skills/botster-install/SKILL.md` — first-agent setup and MCP checks.
- `skills/botster-customize-tui/SKILL.md` — TUI layout/keybinding guidance.
- `skills/botster-customize-hub/SKILL.md` — hub hooks and lifecycle guidance.
- `skills/botster-customize-plugin/SKILL.md` — Botster Lua plugin authoring.
- `skills/botster-customize-mcp/SKILL.md` — MCP tools/prompts from plugins.

## MCP Server

The MCP server forwards `BOTSTER_SESSION_UUID` so the hub can resolve the
calling session:

```json
{
  "mcpServers": {
    "botster": {
      "command": "botster",
      "args": ["mcp-serve"],
      "env_vars": ["BOTSTER_SESSION_UUID"]
    }
  }
}
```

## Install From GitHub

Codex CLI:

```bash
codex plugin marketplace add Tonksthebear/trybotster --ref main --sparse .agents
```

Claude Code:

```text
/plugin marketplace add Tonksthebear/trybotster
/plugin install botster@botster
```
