-- hub/mcp_defaults.lua — Built-in MCP prompts.
--
-- Registers default prompts that ship with Botster out of the box, analogous
-- to how ui/layout.lua and ui/keybindings.lua define the default TUI without
-- preventing customization. Users and plugins can override these by registering
-- a prompt with the same name — the last registration wins.
--
-- These prompts are instruction manuals: an AI agent reads them when the user
-- asks to implement a custom feature, and gets everything needed to do the job.
--
-- Loaded by hub/init.lua after lib.mcp but before user.init, so user overrides
-- take effect naturally.

local mcp = require("lib.mcp")

-- =============================================================================
-- botster-customize-tui
-- =============================================================================

mcp.prompt("botster-customize-tui", {
    description = "How to customize the Botster TUI: change the layout, add keybindings, read agent state",
    arguments = {},
}, function(_args)
    return {
        description = "Botster TUI customization — instruction manual",
        messages = {
            {
                role = "user",
                content = {
                    type = "text",
                    text = [[
The user wants to customize the Botster TUI. Here is everything you need to implement it.

## Where the Code Goes

TUI customizations go in one of these files (only these three names are discovered):

  ~/.botster/lua/user/ui/layout.lua       — replaces or wraps the main layout
  ~/.botster/lua/user/ui/keybindings.lua  — adds or rebinds keys
  ~/.botster/lua/user/ui/actions.lua      — defines custom action handlers

Debug builds use `~/.botster-dev/` instead of `~/.botster/`.

These files load after all built-ins and hot-reload automatically when saved — but the
file-watcher only starts if the file exists when the hub first starts. Create the file,
then restart the hub once; after that, saves hot-reload instantly.

For hub behavior (hooks, commands, external calls), use hub user.init instead:
  ~/.botster/lua/user/init.lua

## How Layout Works

Rust calls two global functions every frame. Override them directly:

  render(state)          → the main layout tree
  render_overlay(state)  → a centered modal on top, or nil

To wrap the built-in layout (add panels, change borders, etc.):

  local orig = render
  render = function(state)
    local tree = orig(state)
    -- modify tree here, or just return a new one
    return tree
  end

To replace the layout entirely, just redefine render:

  render = function(state)
    local agents = (_tui_state and _tui_state.agents) or {}
    return {
      type = "hsplit",
      constraints = { "25%", "75%" },
      children = {
        {
          type = "list",
          block = { title = " Agents ", borders = "all" },
          props = {
            items = (function()
              local out = {}
              for _, a in ipairs(agents) do
                table.insert(out, { text = a.display_name or a.branch_name or "agent" })
              end
              return out
            end)(),
            selected = _tui_state and _tui_state.selected_agent_index,
          },
        },
        {
          type = "terminal",
          block = { title = " Terminal ", borders = "all" },
          props = {
            agent_index = _tui_state and _tui_state.selected_agent_index,
            pty_index   = (_tui_state and _tui_state.active_pty_index) or 0,
          },
        },
      },
    }
  end

## Agent Data (_tui_state)

Read agent and UI state from the _tui_state global inside render():

  _tui_state.agents                  -- array of agent info tables:
    agent.id, agent.display_name, agent.branch_name, agent.profile_name
    agent.sessions, agent.notification, agent.in_worktree
  _tui_state.selected_agent_index    -- 0-based int, or nil
  _tui_state.active_pty_index        -- 0-based int
  _tui_state.available_worktrees     -- array of { branch, path }
  _tui_state.mode                    -- current mode string (see modes below)
  _tui_state.input_buffer            -- current text input
  _tui_state.list_selected           -- 0-based selected list index

## Render State (the `state` argument)

  state.terminal_cols, state.terminal_rows   -- current terminal dimensions
  state.is_scrolled, state.scroll_offset     -- scrollback state
  state.seconds_since_poll                   -- float: age of last hub poll
  state.error_message                        -- string|nil (in error mode)
  state.qr_width, state.qr_height            -- in connection_code mode

## Layout Node Types

  { type = "hsplit",    constraints = {"30%","70%"}, children = { ... } }
  { type = "vsplit",    constraints = {"50%","50%"}, children = { ... } }
  { type = "centered",  width = 60, height = 40, child = { ... } }     -- percentages
  { type = "list",      block = {...}, props = { items = {...}, selected = N } }
  { type = "paragraph", block = {...}, props = { lines = {...}, alignment = "center" } }
  { type = "input",     block = {...}, props = { lines = {...}, placeholder = "..." } }
  { type = "terminal",  block = {...}, props = { agent_index = N, pty_index = N } }
  { type = "empty",     block = {...} }

  Constraints: "30%" | "30" (fixed cols) | "min:10" | "max:80"
  Block: { title = "string or spans", borders = "all" }

## List Items

  { text = "plain string" }
  { text = { {text="A", style="bold"}, {text="B", style="dim"} } }
  { text = "name", secondary = { {text="detail", style="dim"} } }
  { text = "── Section ──", header = true }

  Styles: "bold", "dim", { fg="cyan" }, { fg="yellow", bold=true }
  Colors: "cyan" "yellow" "red" "green" "blue" "white" "gray" "dark_gray" "magenta"

## Modes

  normal                    no agent selected, unbound keys swallowed
  insert                    agent selected, unbound keys forwarded to PTY
  menu                      command palette (Ctrl+P)
  new_agent_select_profile  new_agent_select_worktree  new_agent_create_worktree
  new_agent_prompt          close_agent_confirm         connection_code
  add_session_select_type   remove_session_select       error

## Adding Keybindings

Put this in `~/.botster/lua/user/ui/keybindings.lua`:

  local kb = require("ui.keybindings")
  kb.normal["ctrl+h"] = "show_connection_code"
  kb.insert["ctrl+h"] = "show_connection_code"
  kb.normal["ctrl+n"] = "new_agent"   -- remap existing

  Key format: "a", "enter", "ctrl+p", "shift+enter", "ctrl+]"
  Note: Ctrl+Q is hardcoded in Rust and never reaches Lua.

Built-in shared bindings (normal + insert):
  ctrl+p → open_menu     ctrl+j → select_next     ctrl+k → select_previous
  ctrl+] → toggle_pty    shift+pageup/pagedown/home/end → scroll
  ctrl+r → refresh_agents

## Adding a Custom Overlay

Use render_overlay to show a modal in any mode:

  render_overlay = function(state)
    if _tui_state.mode ~= "my_custom_mode" then return nil end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "paragraph",
        block = { title = " My Modal ", borders = "all" },
        props = { lines = { "Some content" } },
      },
    }
  end
]],
                },
            },
        },
    }
end)

-- =============================================================================
-- botster-customize-hub
-- =============================================================================

mcp.prompt("botster-customize-hub", {
    description = "How to hook into Botster hub events, add hub commands, react to agent lifecycle, and run background tasks",
    arguments = {},
}, function(_args)
    return {
        description = "Botster hub customization — instruction manual",
        messages = {
            {
                role = "user",
                content = {
                    type = "text",
                    text = [[
The user wants to customize how the Botster hub behaves — reacting to events, adding commands, running background work. Here is everything you need to implement it.

## Where the Code Goes

For one-off hub behavior, use user.init (always at this path, debug and release):
  ~/.botster/lua/user/init.lua       (device-wide)

For reusable, distributable features, create a plugin:
  ~/.botster/shared/plugins/{name}/init.lua        (device-wide, release)
  ~/.botster-dev/shared/plugins/{name}/init.lua    (device-wide, debug builds)
  {repo}/.botster/shared/plugins/{name}/init.lua   (repo-specific)

Hot-reload: saved files reload automatically. The exception is new plugin directories —
the watcher only registers directories that exist at hub start. Create the directory
first, restart once, then subsequent file changes hot-reload without restarting.

## Reacting to Agent Lifecycle

  hooks.on("after_agent_create", "my.hook", function(agent)
    -- agent is a live Agent instance
    log.info("Agent started: " .. agent:agent_key())
    local branch = agent:info().branch_name
    -- store custom metadata:
    agent:set_meta("started_at", os.time())
  end)

  hooks.on("after_agent_close", "my.hook_close", function(agent)
    log.info("Agent closed: " .. agent:agent_key())
  end)

  hooks.on("agent_session_added", "my.session", function(payload)
    -- payload.agent, payload.session_name, payload.pty_index
  end)

Available agent observer events:
  after_agent_create    after_agent_close    before_agent_close
  agent_created         agent_deleted        agent_session_added   agent_session_removed

## Adding a Hub Command (Ctrl+P palette)

  commands.register("notify-slack", function(client, sub_id, command)
    log.info("notify-slack invoked")
    -- do async work: http.request(...)
  end, { description = "Send Slack notification" })

The user can invoke this from the TUI command palette (Ctrl+P) or an MCP tool.

## Intercepting Agent Creation

Interceptors run synchronously before the event occurs. Return the (optionally modified) value to allow, return nil to block.

  hooks.intercept("before_agent_create", "my.guard", function(params)
    if not params.branch_name then
      log.warn("Blocking agent creation: no branch")
      return nil   -- blocks creation
    end
    params.display_name = params.branch_name .. " [verified]"
    return params  -- allow with modification
  end, { timeout_ms = 50 })

Available interceptable events:
  before_agent_create    params table    → return modified or nil to block
  before_agent_delete    config table    → return modified or nil to block
  before_command         command table   → return modified or nil to block
  before_client_subscribe context table → return modified or nil to block
  filter_agent_env       (env, agent)    → return modified env (PTY env vars)

## Injecting Environment Variables into Agent PTYs

  hooks.intercept("filter_agent_env", "my.env", function(env, agent)
    env["MY_API_KEY"] = "secret"
    env["AGENT_BRANCH"] = agent:info().branch_name or ""
    return env
  end)

## Running Background Tasks

  local state = require("hub.state")
  local S = state.get("my.state", {})

  if not S._started then
    S._started = true
    S.timer = timer.every(60, function()
      -- runs every 60 seconds, survives as long as the hub is up
      log.info("periodic tick")
    end)
  end

  -- Cancel on hot-reload so the timer doesn't double-register:
  function S._before_reload()
    timer.cancel(S.timer)
    S._started = false
  end

## Persisting State Across Hot-Reloads

hub.state is an in-memory key-value store that survives require() reloads:

  local state = require("hub.state")
  local S = state.get("my_plugin.data", {})   -- same table every call
  S.count = (S.count or 0) + 1
  -- no need to call set() again — S is the live table

## Reacting to Client Connections

  hooks.on("client_connected", "my.conn", function(payload)
    -- payload.peer_id, payload.transport ("webrtc" or "tui")
    log.info("Client connected: " .. payload.peer_id)
  end)

  hooks.on("client_subscribed", "my.sub", function(payload)
    -- payload.peer_id, payload.channel, payload.sub_id
  end)

## Looking Up Agents

  local agents = Agent.list()                   -- all Agent instances
  local agent  = Agent.get("owner-repo-branch") -- by key, or nil
  local found  = Agent.find_by_meta("env", "production")  -- array

  -- On an instance:
  agent:agent_key()          agent:info()
  agent:get_meta("key")      agent:set_meta("key", value)
  agent:close(delete_worktree)

## Making HTTP Requests

  http.request({
    method  = "POST",
    url     = "https://api.example.com/notify",
    headers = { ["Authorization"] = "Bearer " .. token,
                ["Content-Type"]  = "application/json" },
    body    = json.encode({ text = "Agent started" }),
  }, function(resp, err)
    if err then
      log.warn("HTTP error: " .. tostring(err))
      return
    end
    if resp.status ~= 200 then
      log.warn("HTTP error: " .. tostring(resp.status))
    end
  end)

## Storing Secrets

  secrets.set("my-plugin", "api_token", "the-value")   -- call once to store
  local token, err = secrets.get("my-plugin", "api_token")

## Connecting to External Services

ActionCable (Rails):
  local conn = action_cable.connect()  -- connects to the hub's ActionCable endpoint
  local ch = action_cable.subscribe(conn, "MyChannel", { room = "x" },
    function(msg, channel_id)
      log.info(json.encode(msg))
    end)
  action_cable.perform(ch, "my_action", { payload = "..." })
  action_cable.close(conn)          -- closes connection and all its channels
  -- action_cable.unsubscribe(ch)   -- or unsubscribe a single channel

  -- With E2E crypto (auto-decrypts signal envelopes on this connection):
  local crypto_conn = action_cable.connect({ crypto = true })

Raw WebSocket:
  local ws, err = websocket.connect("wss://...", {
    on_open    = function() log.info("connected") end,
    on_message = function(msg) log.info(msg) end,
    on_close   = function(code, reason) log.info("closed: " .. reason) end,
    on_error   = function(e) log.warn("ws error: " .. e) end,
  })
  if err then log.error("websocket.connect failed: " .. tostring(err)) return end
  websocket.send(ws, "hello")
  websocket.close(ws)
]],
                },
            },
        },
    }
end)

-- =============================================================================
-- botster-create-plugin
-- =============================================================================

mcp.prompt("botster-create-plugin", {
    description = "Step-by-step guide to creating a Botster plugin: file location, secrets, HTTP, timers, hooks, MCP tools, and hot-reload",
    arguments = {
        { name = "scope",   description = "Where the plugin lives: 'device' or 'repo'",                required = false },
        { name = "profile", description = "Profile name to scope to, or omit for all profiles",         required = false },
    },
}, function(args)
    local scope   = args.scope or "device"
    local profile = args.profile
    local base    = scope == "repo" and "{repo}/.botster" or "~/.botster"
    local layer   = (profile and profile ~= "")
        and (base .. "/profiles/" .. profile .. "/plugins/{name}")
        or  (base .. "/shared/plugins/{name}")

    return {
        description = "Botster plugin creation — instruction manual",
        messages = {
            {
                role = "user",
                content = {
                    type = "text",
                    text = string.format([[
The user wants to create a Botster plugin. Here is everything you need to build and wire it up.

## Step 1: Create the File

  %s/init.lua

Device scope: active on every project on this machine.
  Release: ~/.botster/shared/plugins/{name}/init.lua
  Debug:   ~/.botster-dev/shared/plugins/{name}/init.lua

Repo scope: active only when working in that repo.
  {repo}/.botster/shared/plugins/{name}/init.lua

Shared: active for all profiles. Profile-scoped: only when that profile is selected.
  Replace "shared" with "profiles/{profile}" for profile-specific plugins.

Botster auto-discovers the file when the hub starts (or hot-reloads if already running).
The file executes top-to-bottom on load. No registration step — just create and save.
Note: new plugin directories must exist at hub start for hot-reload to work — create the
directory, restart once, then file changes reload automatically.

## Step 2: Decide What the Plugin Does

Most plugins combine some of these building blocks:

  Credentials      → secrets.get / secrets.set
  Outbound HTTP    → http.request
  Persistent conn  → action_cable.subscribe or websocket.connect
  Polling          → timer.every (with hot-reload guard)
  React to agents  → hooks.on("after_agent_create", ...)
  Expose to Claude → mcp.tool / mcp.prompt
  TUI palette      → commands.register

## Step 3: Store Credentials

Never hardcode tokens. Use the encrypted secrets store:

  -- To write (do this once, e.g. from a setup tool):
  secrets.set("my-plugin", "bot_token", "actual-token-value")

  -- To read at load time:
  local token, err = secrets.get("my-plugin", "bot_token")
  if not token then
    log.error("my-plugin: missing bot_token — run setup first")
    return   -- abort plugin load gracefully
  end

## Step 4: Make HTTP Requests

  http.request({
    method  = "GET",
    url     = "https://api.example.com/endpoint",
    headers = { ["Authorization"] = "Bearer " .. token },
  }, function(resp, err)
    if err then
      log.warn("my-plugin: HTTP error: " .. tostring(err))
      return
    end
    if resp.status == 200 then
      local data = json.decode(resp.body)
      -- handle data
    else
      log.warn("my-plugin: HTTP " .. resp.status)
    end
  end)

http.request is non-blocking. The callback fires on the Lua event loop.

## Step 5: Poll on a Timer (Hot-Reload Safe)

Don't register a timer at the top level — it re-registers every reload.
Use the state guard pattern:

  local state = require("hub.state")
  local S = state.get("my-plugin.state", {})   -- same table across reloads

  if not S._started then
    S._started = true
    S.poll_timer = timer.every(30, function()
      -- runs every 30 seconds
      http.request({ method = "GET", url = "https://...", headers = {} }, function(resp, err)
        if err then log.warn("my-plugin: poll error: " .. tostring(err)) return end
        -- handle updates
      end)
    end)
  end

  function S._before_reload()
    timer.cancel(S.poll_timer)
    S._started = false
  end

## Step 6: React to Agent Events

  hooks.on("after_agent_create", "my-plugin.agent-start", function(agent)
    local info = agent:info()
    log.info("my-plugin: agent started on branch " .. (info.branch_name or "unknown"))
    -- notify an external service, update state, etc.
  end)

  hooks.on("after_agent_close", "my-plugin.agent-stop", function(agent)
    log.info("my-plugin: agent closed: " .. agent:agent_key())
  end)

Other useful agent events:
  before_agent_close    agent_session_added    agent_session_removed

## Step 7: Expose Tools to Claude

  mcp.tool("send_message", {
    description = "Send a message to the external service",
    input_schema = {
      type = "object",
      properties = {
        text = { type = "string", description = "Message to send" },
      },
      required = { "text" },
    },
  }, function(params, context)
    -- context.agent_key identifies the calling agent
    local ok = send_to_service(params.text)
    return ok and "Sent." or "Failed to send."
  end)

## Step 8: Add to TUI Command Palette

  commands.register("my-plugin-action", function(client, sub_id, command)
    log.info("my-plugin-action invoked")
    -- do work
  end, { description = "My plugin action" })

The user can invoke this with Ctrl+P → "my-plugin-action".

## Step 9: Expose a Prompt to Claude

  mcp.prompt("my-plugin-context", {
    description = "Inject current plugin state into the conversation",
    arguments = {},
  }, function(_args)
    local S = require("hub.state").get("my-plugin.state", {})
    return string.format(
      "Plugin status: %s\nLast update: %s",
      S.status or "unknown",
      tostring(S.last_updated or "never")
    )
  end)

## Complete Example: Telegram Notifier

  -- %s/init.lua

  local state = require("hub.state")
  local S = state.get("telegram.state", {})

  -- Load credentials
  local token, _ = secrets.get("telegram", "bot_token")
  local chat_id, _ = secrets.get("telegram", "chat_id")

  if not token or not chat_id then
    log.error("telegram plugin: set bot_token and chat_id in secrets first")
    return
  end

  local function send(text)
    http.request({
      method = "POST",
      url    = "https://api.telegram.org/bot" .. token .. "/sendMessage",
      headers = { ["Content-Type"] = "application/json" },
      body   = json.encode({ chat_id = chat_id, text = text }),
    }, function(resp, err)
      if err then
        log.warn("telegram: send error: " .. tostring(err))
        return
      end
      if resp.status ~= 200 then
        log.warn("telegram: send failed: " .. resp.status)
      end
    end)
  end

  -- Notify on agent start/stop
  hooks.on("after_agent_create", "telegram.agent-start", function(agent)
    local info = agent:info()
    send("Agent started: " .. (info.display_name or info.branch_name or agent:agent_key()))
  end)

  hooks.on("after_agent_close", "telegram.agent-stop", function(agent)
    send("Agent closed: " .. agent:agent_key())
  end)

  -- MCP tool so Claude can send messages directly
  mcp.tool("telegram_send", {
    description = "Send a Telegram message to the configured chat",
    input_schema = {
      type = "object",
      properties = {
        text = { type = "string", description = "Message text" },
      },
      required = { "text" },
    },
  }, function(params, _ctx)
    send(params.text or "")
    return "Sent."
  end)

  log.info("telegram plugin loaded")

## Hot-Reload Notes

- Save the file → it reloads automatically in ~1 second
- Top-level code re-runs on every reload
- Use state.get() + the _started guard for timers and one-time setup
- Use function S._before_reload() to cancel timers before reload
- MCP tools and hooks are automatically cleared before reload and re-registered after
]], layer, layer),
                },
            },
        },
    }
end)

-- =============================================================================
-- botster-customize-mcp
-- =============================================================================

mcp.prompt("botster-customize-mcp", {
    description = "How to expose MCP tools and prompts to Claude from Botster plugins",
    arguments = {},
}, function(_args)
    return {
        description = "Botster MCP tools and prompts — instruction manual",
        messages = {
            {
                role = "user",
                content = {
                    type = "text",
                    text = [[
The user wants to add MCP tools or prompts so Claude can interact with the hub. Here is everything you need to implement it.

## Where the Code Goes

From a plugin (recommended for anything reusable):
  ~/.botster/shared/plugins/{name}/init.lua        (device-wide, release)
  ~/.botster-dev/shared/plugins/{name}/init.lua    (device-wide, debug builds)
  {repo}/.botster/shared/plugins/{name}/init.lua   (repo-specific)

From user.init (good for quick one-offs — same path in debug and release):
  ~/.botster/lua/user/init.lua

All registrations appear together in Claude's tool/prompt list. Last write for a given name wins.

## Registering a Tool

A tool lets Claude call into the hub and get a result.

  mcp.tool("tool_name", {
    description = "One-line description shown in Claude",
    input_schema = {
      type = "object",
      properties = {
        file    = { type = "string",  description = "Path to read"       },
        limit   = { type = "number",  description = "Max results"        },
        verbose = { type = "boolean", description = "Include extra info" },
      },
      required = { "file" },
    },
  }, function(params, context)
    -- params.file, params.limit, params.verbose — values from Claude
    -- context.agent_key   — key of the agent Claude is running in
    -- context.hub_id      — hub UUID
    -- context.caller_cwd  — working directory of the calling process

    -- Return a string, a plain table (auto JSON-encoded), or an MCP content array:
    return { result = "data", count = 3 }   -- becomes JSON text in Claude
  end)

### Using Hub Data in a Tool

Anything available in Lua is available in the handler:

  mcp.tool("list_agents", {
    description = "List all running agents with their metadata",
    input_schema = { type = "object", properties = {} },
  }, function(params, context)
    return Agent.all_info()   -- auto JSON-encoded
  end)

  mcp.tool("worktree_status", {
    description = "List all git worktrees",
    input_schema = { type = "object", properties = {} },
  }, function(params, context)
    return worktree.list()
  end)

  mcp.tool("read_agent_file", {
    description = "Read a file from the calling agent's worktree",
    input_schema = {
      type = "object",
      properties = { path = { type = "string", description = "Relative file path" } },
      required = { "path" },
    },
  }, function(params, context)
    local agent = Agent.get(context.agent_key)
    if not agent then return "No agent context." end
    local wt_path = agent:info().worktree_path
    if not wt_path then return "Agent has no worktree." end
    local content, err = fs.read(wt_path .. "/" .. params.path)
    return content or ("Error: " .. tostring(err))
  end)

### Signaling Errors

Return a descriptive string on soft errors. For hard errors that Claude should see as isError=true, use error():

  if not ok then error("Failed to read file: " .. path) end

## Registering a Prompt

A prompt is a template Claude can select to inject context into the conversation.

  mcp.prompt("prompt-name", {
    description = "One-line description shown in Claude",
    arguments = {
      { name = "focus", description = "What area to focus on", required = false },
    },
  }, function(args)
    -- Return a plain string (auto-wrapped as a user message):
    return "Here is the current hub state: ..."

    -- Or return a full multi-turn shape:
    return {
      description = "Hub context for this task",
      messages = {
        { role = "user",      content = { type = "text", text = "Current agents: ..." } },
        { role = "assistant", content = { type = "text", text = "Understood." } },
        { role = "user",      content = { type = "text", text = args.focus or "Begin." } },
      },
    }
  end)

### Building a Useful Context Prompt

  mcp.prompt("hub-context", {
    description = "Inject current hub state — agents and worktrees — as conversation context",
    arguments = {},
  }, function(_args)
    local agents = Agent.all_info()
    local wts    = worktree.list() or {}

    local agent_lines = {}
    for _, a in ipairs(agents) do
      table.insert(agent_lines, string.format(
        "  - %s  branch=%s  status=%s",
        a.display_name or a.id,
        a.branch_name or "none",
        a.status or "unknown"
      ))
    end

    local wt_lines = {}
    for _, wt in ipairs(wts) do
      table.insert(wt_lines, "  - " .. wt.branch .. "  at " .. wt.path)
    end

    return {
      description = "Current Botster hub state",
      messages = {
        {
          role = "user",
          content = { type = "text", text = table.concat({
            "## Hub State",
            "",
            "Agents (" .. #agents .. "):",
            #agent_lines > 0 and table.concat(agent_lines, "\n") or "  (none)",
            "",
            "Worktrees (" .. #wts .. "):",
            #wt_lines > 0 and table.concat(wt_lines, "\n") or "  (none)",
          }, "\n") },
        },
      },
    }
  end)

## Removing a Registration

  mcp.remove_tool("tool_name")
  mcp.remove_prompt("prompt-name")

On hot-reload, Botster automatically clears and re-registers everything from the reloaded file.
Manual removal is only needed if you want to conditionally unregister at runtime.

## Naming Conventions

  Tools:   snake_case  — "list_agents", "get_worktree_status", "run_tests"
  Prompts: kebab-case  — "hub-context", "start-task", "code-review"

## Checking What's Registered

  mcp.list_tools()    -- array of { name, description, input_schema }
  mcp.list_prompts()  -- array of { name, description, arguments }
  mcp.count()         -- int
  mcp.count_prompts() -- int
]],
                },
            },
        },
    }
end)

log.info("MCP default prompts registered: botster-customize-tui, botster-customize-hub, botster-create-plugin, botster-customize-mcp")
