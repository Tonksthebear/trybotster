# Session Actions

Session actions are the generic capability model for plugin-owned affordances
on a Botster session. Core owns the registry, entity publication, and invocation
command. Plugins own the external policy and side effects.

`session_action` is a built-in entity family for per-session capabilities. Use
plugin-owned entities such as `project-pipelines.ticket` or `vault.card` for
arbitrary plugin domain records; do not overload `session_action` for general
plugin state. Both models share the same client entity stream, but
`session_action` descriptors are joined to core sessions by `session_uuid` and
invoked through `execute_session_action`.

Use this model when a plugin wants clients to show a per-session action such as
opening a provider URL, toggling a connector, retrying a workflow, or launching a
plugin-specific task. Do not add client-specific commands or provider-specific hub
commands for those cases.

## Register

```lua
local session_actions = require("lib.session_actions")

session_actions.register("example.task.toggle", {
  plugin = "example",
  label = function(session)
    return session.example_task and "Stop task" or "Start task"
  end,
  status = function(session)
    return session.example_task and session.example_task.status or "inactive"
  end,
  url = function(session)
    return session.example_task and session.example_task.url or nil
  end,
  error = function(session)
    return session.example_task and session.example_task.error or nil
  end,
  icon = "sparkle",
  visibility = function(session)
    return session.port ~= nil
  end,
  enabled = function(session)
    return session.status == "running"
  end,
  run = function(session_uuid, action_id, context)
    -- Queue long-running work and return promptly.
    -- context.params carries client-supplied invocation parameters.
  end,
})
```

Descriptor fields can be literal values or functions that receive
`(session, action_id)`. `visibility = nil` and `visibility = true` publish as
`"visible"`; `visibility = false` publishes as `"hidden"`.

## Descriptor Shape

Core publishes descriptors as `session_action` entities:

```lua
{
  id = "sess-123:example.task.toggle",
  session_uuid = "sess-123",
  action_id = "example.task.toggle",
  label = "Start task",
  status = "inactive",
  icon = "sparkle",
  visibility = "visible",
  enabled = true,
  plugin = "example",
}
```

Clients consume these descriptors from the entity stream, keyed by `id`, and
join them to sessions by `session_uuid`. That keeps browser and TUI clients on
the same contract.

Besides the core fields above, plugins may add descriptor fields such as `url`,
`link_url`, `install_url`, or `error`. Those fields may also be literal values
or functions receiving `(session, action_id)`.

## Invoke

Clients invoke actions through the generic hub command:

```lua
{
  type = "execute_session_action",
  session_uuid = "sess-123",
  action_id = "example.task.toggle",
  params = { enabled = true },
}
```

The registry rejects invocations when the action is not registered, the session
does not exist, the descriptor is hidden, or the descriptor is disabled. Handler
functions receive `(session_uuid, action_id, context)` where `context.session`
is the current session info and `context.action` is the descriptor that passed
the visibility/enabled checks.

Use `session_uuid` for all public identifiers. `id` is only the entity-store key
for a `session_action` descriptor.

Submit-style UI controls inside Lua-authored plugin surfaces use the generic
`ui_action` / `ui_action_result` lifecycle documented in
[`primitives.md`](primitives.md). That lifecycle is separate from
`execute_session_action`: use `ui_action` for surface buttons and
`session_action` for reusable per-session capabilities.

## Runtime Work

Action handlers run on the hub command path. They should validate current state,
update plugin-owned session state, queue blocking or long-running work, and
return promptly. Return `nil, "message"` or `false, "message"` when validation
fails; core will surface the error through the generic command error path.

When an action needs to resolve a local executable or materialize a small config
file before spawning a connector session, use `hub.prepare_plugin_command(...)`
instead of direct `fs`/`io` calls in the action handler. The hub offloads the
blocking filesystem/PATH work and emits `plugin_command_prepared` with the
original `request_id`, opaque `context`, and a structured `error_kind`:

- `command_blank`
- `command_missing`
- `config_write_failed`
- `task_failed`

Plugins should store a plugin-owned request token in `plugin_state` and ignore
completion events whose `request_id` is no longer current. This prevents stale
work from resurrecting a connector after disable, retry, session close, or
plugin reload.
