# Core Lua and Product Policy Boundaries

Core Lua is the hub framework. It owns primitives, registries, lifecycle hooks,
transport wiring, persistence helpers, and generic UI surfaces. It should not
own product-specific integrations such as GitHub workflows, agent-brand setup,
external notification services, or opinionated template catalogs.

## Core Lua Owns

- Stable primitives exposed by Rust, such as `fs`, `http`, `pty`, `secrets`,
  `worktree`, `mcp`, `watch`, and transport primitives.
- Generic registries and protocols, such as hooks, commands, surfaces, plugin
  loading, entity broadcasts, template install/list/uninstall commands, and MCP
  prompt/tool registration.
- Built-in framework surfaces required for Botster itself, such as workspace
  sidebar/panel surfaces.
- Compatibility migrations for old hub data, even when the historical data used
  product-specific prefixes. Those branches are legacy readers, not precedent
  for new product behavior in core.

## Plugins and Templates Own

- GitHub, issue tracker, chat, notification, hosted-preview, and similar
  integration behavior.
- Product-specific setup flows, credentials, prompts, and external API policy.
- Agent and accessory definitions for specific tools or brands.
- Catalog choices for which templates are offered to users.

## Rails Owns

Rails may serve a generic template catalog reader and return template files with
metadata and content intact. It should not decide which integrations belong in
the product. Template entries can move to a static directory or synced source
without changing hub core semantics.

## Rule of Thumb

If behavior can be expressed as `hooks.on`, `commands.register`,
`surfaces.register`, `mcp.tool`, `mcp.prompt`, or a template file, keep it out of
core Lua unless Botster cannot boot or coordinate sessions without it.
