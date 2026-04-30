# Core Lua and Product Policy Boundaries

Core Lua is the hub framework. It owns primitives, registries, lifecycle hooks,
transport wiring, persistence helpers, and generic UI surfaces. It should not
own product-specific integrations such as GitHub workflows, agent-brand setup,
external notification services, or opinionated template catalogs.

## Core Lua Owns

- Stable primitives exposed by Rust, such as `fs`, `http`, `pty`, `secrets`,
  `worktree`, `mcp`, `watch`, and transport primitives.
- Generic registries and protocols, such as hooks, commands, surfaces, plugin
  loading, entity broadcasts, template catalog providers,
  template install/list/uninstall commands, MCP prompt/tool registration, and
  session action descriptor publication.
- Generic readiness/lifecycle primitives that plugin capabilities compose, such
  as hidden accessory orchestration, parent-session metadata, and
  `hub.prepare_plugin_command(...)` / `hub.probe_url_ready(...)`.
- Built-in framework surfaces required for Botster itself, such as workspace
  sidebar/panel surfaces.
- Current data migrations required for live hub data. Historical layout readers
  should not stay in core once the project chooses a cold turkey switch.

## Plugins and Templates Own

- GitHub, issue tracker, chat, notification, hosted-preview providers, and
  similar integration behavior. These integrations expose user affordances as
  plugin-owned session actions instead of adding bespoke core commands.
- Connector process policy, including command names, install links, generated
  config contents, retry behavior, and provider-specific readiness state.
- Product-specific setup flows, credentials, prompts, and external API policy.
- Agent and accessory definitions for specific tools or brands.
- Catalog choices for which templates are offered to users.

## Rails Owns

Rails owns settings bootstrap, authentication, and the browser shell. It must
not discover, fetch, or parse template catalogs. Browsers consume the hub's
`template` entity snapshot. Remote sources such as the default GitHub-backed
trybotster catalog belong behind the hub catalog provider/cache rather than a
Rails controller.

## Rule of Thumb

If behavior can be expressed as `hooks.on`, `commands.register`,
`surfaces.register`, `session_actions.register`, `mcp.tool`, `mcp.prompt`, or a
template file, keep it out of core Lua unless Botster cannot boot or coordinate
sessions without it.
