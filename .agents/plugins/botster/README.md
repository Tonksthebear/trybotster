# Botster Agent Plugin

This package is the agent-side Botster integration. It installs the Botster MCP
server configuration and ships Botster workflow skills so agents do not need
manual MCP setup or generic default MCP prompt discovery.

Botster Lua plugins still own the runtime tool surface. This package only
connects the agent to `botster mcp-serve` and teaches the agent how to use the
hub, session, messaging, and orchestration tools correctly.

The same `skills/` directory is shared by the Codex and Claude plugin manifests
in this package. Update those skill files once and both agent runtimes receive
the same Botster authoring guidance when the plugin is installed from this
source.

## Included

- Version `0.1.10` — plugin authoring guidance includes scoped notification
  policy ownership, all-session capability gates, and plugin-worker execution
  boundaries for notification handlers.
- `.codex-plugin/plugin.json` — plugin manifest.
- `.claude-plugin/plugin.json` — Claude Code plugin manifest.
- `.mcp.json` — single MCP server named `botster`.
- `skills/botster-install/SKILL.md` — first-agent setup and MCP checks.
- `skills/botster-customize-tui/SKILL.md` — TUI layout/keybinding guidance.
- `skills/botster-customize-hub/SKILL.md` — hub hooks and lifecycle guidance.
- `skills/botster-customize-plugin/SKILL.md` — Botster Lua plugin authoring.
  Includes browser surface registration, core plugin navigation, Heroicons icon
  names, route-scoped plugin sidebars, plugin-owned session metadata, and
  surface-local terminal routing, plus sandboxed custom HTML views through
  plugin assets, iframes, fullscreen plugin route layouts, and scoped
  notification policy ownership.
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
      "env_vars": ["BOTSTER_SESSION_UUID"],
      "default_tools_approval_mode": "approve",
      "default_tools_enabled": true
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
