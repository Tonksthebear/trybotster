# Lua Architecture Vision

## Why: Self-Improving Agents

The core goal is enabling botster agents to improve botster itself—and see those improvements working immediately, in the same session.

Today's problem:
```
Agent improves hub code
    → Requires Rust recompile
    → Requires hub restart
    → Kills all running agents
    → macOS keyring prompts again
    → Agent can't see its changes working
```

The vision:
```
Agent improves hub code (Lua)
    → Hub hot-reloads
    → No restart, no recompile
    → Agents keep running
    → No keyring prompt (binary unchanged)
    → Agent sees its improvement immediately
```

This feedback loop is the entire point. Everything else follows from it.

---

## Architecture Overview

### The Split

**Rust Runtime (frozen, rarely changes):**
- Lua runtime (`mlua` with Lua 5.4)
- Event loop (tokio)
- PTY spawn/management
- Terminal I/O (crossterm)
- WebRTC DataChannel
- Crypto (ring)
- HTTP/WebSocket server
- Keyring access
- Git operations (git2)

**Lua Layer (changes constantly, hot-reloadable):**
- Agent lifecycle management
- Event handlers / hooks
- TUI layout and logic
- Web routes and templates
- State management
- Business logic
- User customizations

### Why This Split?

Rust handles things that:
1. **Require FFI** — PTY, keyring, system APIs
2. **Are security-critical** — Crypto must use audited libraries
3. **Need raw performance** — Event loop, byte streaming
4. **Are complex protocols** — WebRTC

Lua handles things that:
1. **Change often** — Business logic, UI, behavior
2. **Users customize** — Templates, hooks, routes
3. **Agents improve** — The self-improvement target
4. **Benefit from hot reload** — Everything above

### The Mental Model

```
┌─────────────────────────────────────────────────────────────┐
│                        LUA LAYER                            │
│                                                             │
│   Agent lifecycle       Event handlers      Business logic  │
│   TUI layout/widgets    Web routes          State mgmt      │
│   User customizations   Self-improvements                   │
│                                                             │
│   ↓ calls primitives ↓                                      │
├─────────────────────────────────────────────────────────────┤
│                   RUST RUNTIME (frozen)                     │
│                                                             │
│   mlua          tokio        PTY           crossterm        │
│   WebRTC        ring         reqwest       git2             │
│   keyring       HTTP server  WebSocket                      │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

The Rust binary is like Node.js or the JVM—infrastructure that rarely changes. The Lua layer is the actual application.

---

## Rails' Role

Rails continues to handle:
- **P2P negotiation** — WebRTC signaling
- **GitHub event forwarding** — Webhook ingestion, routing to hubs
- **Auth & billing** — User accounts, subscriptions
- **Hub registry** — Which hubs exist, their status
- **Fundamental web pages** — Landing, settings, hub overview

Rails provides concrete interfaces for hub interaction. The hub serves its own workspace UI, but Rails remains the coordination layer.

---

## Phase 1: Rust Runtime + Lua Primitives

### Goal

Create a minimal Rust runtime that exposes primitives to Lua. Prove the hot-reload loop works with one meaningful component.

### Suggested First Target: Client/Connection Management

Currently in Rust, could become Lua:
- Client state tracking
- Connection lifecycle
- Reconnection logic
- Status updates

This is self-contained, not performance-critical, and exercises the Rust↔Lua interface.

### Primitives to Expose

```lua
-- PTY
pty.spawn(cmd, { args = {}, env = {}, cwd = "" })
pty:write(data)
pty:resize(cols, rows)
pty:kill()
pty:on_output(callback)
pty:on_exit(callback)

-- Terminal (for TUI)
term.size()
term.clear()
term.set_cursor(x, y)
term.write(text, { fg = "white", bg = "black", bold = false })
term.flush()

-- Events
events.on(event_type, callback)
events.off(subscription_id)
events.emit(event_type, payload)

-- HTTP Client
http.get(url, { headers = {} })
http.post(url, { json = {}, headers = {} })

-- WebSocket
ws.connect(url)
ws:send(data)
ws:on_message(callback)
ws:close()

-- WebRTC (higher-level)
webrtc.connect(signal_url, token)
webrtc:send(channel, data)
webrtc:on_message(callback)

-- Crypto
crypto.encrypt(plaintext, key)
crypto.decrypt(ciphertext, key)
crypto.random_bytes(n)

-- Keyring
keyring.get(service, key)
keyring.set(service, key, value)

-- Git
git.worktree_add(repo, path, branch)
git.worktree_remove(path)
git.status(path)

-- File watching (for hot reload)
watch.directory(path, callback)
```

### The Rust↔Lua Interface

```rust
use mlua::prelude::*;

fn create_lua_runtime() -> Lua {
    let lua = Lua::new();

    // Expose PTY primitive
    lua.globals().set("pty", lua.create_table_from([
        ("spawn", lua.create_function(|_, (cmd, opts): (String, LuaTable)| {
            // Rust implementation
            let pty = spawn_pty(&cmd, &opts)?;
            Ok(PtyHandle::new(pty))
        })?),
    ])?)?;

    // Expose other primitives...

    lua
}
```

### Success Criteria

1. Rust binary starts, loads Lua
2. One component (e.g., client management) runs in Lua
3. Modify the Lua file → hub hot-reloads → behavior changes
4. No restart, no recompile, agents keep running

---

## Phase 2: TUI in Lua

### Goal

Move TUI layout and logic to Lua while keeping terminal rendering in Rust.

### Approach

Expose ratatui-like widgets to Lua:

```lua
function render(frame)
    local chunks = layout.vertical({ "3", "1fr", "3" }, frame.size)

    frame:render(widgets.block({
        title = "Botster Hub",
        borders = "all",
    }), chunks[1])

    frame:render(widgets.list(
        map(state.agents, agent_to_list_item),
        { selected = state.selected_index }
    ), chunks[2])

    frame:render(widgets.paragraph(
        "Agents: " .. #state.agents
    ), chunks[3])
end
```

Or a higher-level declarative API:

```lua
function App:render()
    return ui.column({
        ui.header({ title = "Botster Hub" }),
        ui.agent_list(self.agents, { selected = self.selected }),
        ui.status_bar({ text = "Agents: " .. #self.agents }),
    })
end
```

### User Customization

Users override layout by providing their own render function:

```lua
-- user/layout.lua
function App:render()
    -- Completely custom layout
    return ui.row({
        ui.panel({ width = "30%" }, ui.agent_list(self.agents)),
        ui.panel({ width = "70%" }, ui.terminal(self.focused_agent)),
    })
end
```

### Success Criteria

1. TUI renders from Lua
2. Modify layout Lua → hot-reload → UI changes
3. User can override default layout
4. Performance feels identical to current Rust TUI

---

## Phase 3: Web Customizability

### Goal

Hub serves its own web UI using Hotwire (Turbo + Stimulus), customizable via Lua and templates.

### Architecture

```
Browser
├── Turbo (Drive, Frames, Streams)
├── Stimulus controllers
├── botster.js (WebSocket connection)
└── User's custom JS (optional)
        │
        │ WebSocket
        ▼
Hub (Lua)
├── routes.lua (HTTP endpoints)
├── hooks.lua (Turbo Stream broadcasts)
├── templates/*.liquid
└── static/controllers/*.js
```

### Lua Routes

```lua
routes.get("/dashboard", function(req)
    return render("dashboard.liquid", {
        agents = hub:list_agents(),
        user = req.user,
    })
end)

routes.post("/agents/:id/kill", function(req)
    hub:kill_agent(req.params.id)
    return redirect("/dashboard")
end)
```

### Turbo Streams from Hooks

```lua
hooks.register("on_agent_spawned", "turbo_broadcast", function(agent)
    web.turbo_stream("append", "agents",
        render("_agent_card.liquid", { agent = agent })
    )
end)

hooks.register("on_agent_output", "turbo_broadcast", function(agent, output)
    web.turbo_stream("append", "terminal-" .. agent.id,
        render("_terminal_output.liquid", { output = output })
    )
end)
```

### Stimulus Controllers

Ship default controllers for terminal, agent cards, etc. Users can override:

```javascript
// user/controllers/agent_controller.js
import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
    celebrate() {
        confetti({ particleCount: 100 })
    }
}
```

### User Customization

**Templates:**
```liquid
<!-- user/templates/dashboard.liquid -->
<div class="my-custom-dashboard">
    <!-- Their design -->
</div>
```

**Routes:**
```lua
-- user/routes.lua
routes.get("/dashboard", function(req)
    return render("user/my_dashboard.liquid", { ... })
end)
```

**Hooks:**
```lua
-- user/hooks.lua
hooks.register("on_agent_exited", "my_notification", function(agent)
    -- Custom behavior
end)
```

### Success Criteria

1. Hub serves functional web UI with Hotwire
2. Real-time updates via Turbo Streams
3. User can override templates, routes, hooks
4. Stimulus controllers work for interactivity

---

## Phase 4: Self-Improvement Infrastructure

### Goal

Complete the feedback loop: agents can write Lua, hub loads it, agent sees it working.

### Components

**File Watching:**
```lua
watch.directory("~/.botster/improvements", function(path, event)
    if event == "modified" or event == "created" then
        hub:reload_file(path)
    end
end)
```

**Safe Reload:**
```lua
function hub:reload_file(path)
    -- Preserve state
    local state = self:serialize_state()

    -- Load new code
    local ok, err = pcall(dofile, path)
    if not ok then
        log.error("Failed to load %s: %s", path, err)
        return false
    end

    -- Restore state
    self:restore_state(state)
    log.info("Reloaded: %s", path)
    return true
end
```

**Hook System:**
```lua
-- Agents can register new hooks
hooks.register("on_agent_output", "my_improvement", function(agent, output)
    -- Agent-written logic
end)

-- Or replace existing ones
hooks.unregister("on_agent_spawn", "default_scheduler")
hooks.register("on_agent_spawn", "smarter_scheduler", function(...)
    -- Better logic
end)
```

### The Loop

```
1. Agent identifies improvement opportunity
2. Agent writes Lua file to improvements/
3. File watcher triggers reload
4. New behavior is active
5. Agent tests its improvement
6. Agent iterates
7. All in one session, no restarts
```

### Safety Considerations

For agent-written code, consider:
- Resource limits (memory, CPU time)
- Restricted primitives (no keyring access for agent code?)
- Rollback on error

For user-written code, full trust is fine.

### Success Criteria

1. File changes trigger hot reload
2. Agents can write Lua that gets loaded
3. State preserved across reloads
4. Agent can verify its changes work

---

## Migration Strategy

### Principles

1. **Incremental** — Both systems coexist during transition
2. **Component by component** — Migrate one piece at a time
3. **Tests at each step** — Verify behavior matches before/after
4. **Rollback capability** — Can disable Lua for a component if issues

### Suggested Order

```
Phase 1: Primitives + one component
├── Build Rust runtime with mlua
├── Expose basic primitives
├── Migrate client/connection management to Lua
└── Prove hot-reload works

Phase 2: More backend logic
├── Agent lifecycle in Lua
├── Event handlers in Lua
├── State management in Lua
└── Hub mesh communication in Lua

Phase 3: TUI
├── Expose widget primitives
├── Migrate layout to Lua
├── Add user customization points
└── Verify performance

Phase 4: Web
├── Add HTTP/WebSocket primitives
├── Lua routes + Liquid templates
├── Turbo Stream integration
├── Stimulus controllers

Phase 5: Self-improvement
├── File watching
├── Hot reload infrastructure
├── Agent code loading
└── Complete the loop
```

### Coexistence During Migration

```rust
// Rust can call Lua or use native impl
fn handle_client_connect(&mut self, client: Client) {
    if self.lua_enabled("client_management") {
        self.lua.call("on_client_connect", client)?;
    } else {
        // Original Rust implementation
        self.clients.insert(client.id, client);
    }
}
```

Feature flags per component allow gradual rollout.

---

## File Structure

### Shipped with Binary

```
~/.botster/
├── core/                      # Core Lua (shipped, rarely modified)
│   ├── runtime.lua            # Event loop, hot reload
│   ├── hooks.lua              # Hook system
│   ├── agent.lua              # Agent abstraction
│   ├── tui/
│   │   ├── init.lua
│   │   ├── widgets.lua
│   │   └── layout.lua
│   └── web/
│       ├── routes.lua
│       ├── turbo.lua
│       └── templates/
│
├── default/                   # Default behaviors (overridable)
│   ├── hooks.lua
│   ├── layout.lua
│   ├── keybindings.lua
│   └── web/
│       ├── routes.lua
│       └── templates/
│
├── user/                      # User customizations
│   ├── hooks.lua
│   ├── layout.lua
│   ├── routes.lua
│   └── templates/
│
└── improvements/              # Agent-written code
    └── *.lua
```

### Load Order

```lua
-- Boot sequence
require("core.runtime")
require("core.hooks")
require("core.agent")
require("core.tui")
require("core.web")

require("default.hooks")
require("default.layout")
require("default.keybindings")
require("default.web.routes")

pcall(require, "user.hooks")
pcall(require, "user.layout")
pcall(require, "user.routes")

for _, file in ipairs(glob("improvements/*.lua")) do
    pcall(dofile, file)
end

hub:run()
```

---

## Open Questions

1. **Debugging Lua** — What's the debugging story? Print statements? DAP integration?

2. **Error handling** — When Lua errors, how does the hub recover gracefully?

3. **Testing Lua** — Use busted? How to test TUI components?

4. **Versioning** — How to handle core Lua updates vs user customizations?

5. **Performance monitoring** — How to identify if Lua is a bottleneck?

---

## Not In Scope

- Multi-hub encrypted sync (separate initiative)
- WASM plugin system (Lua customization is sufficient)
- Moving auth/billing from Rails

---

## Success Metrics

The architecture is successful when:

1. **An agent can improve the hub and see it working in the same session**
2. **Users can customize TUI and web without touching Rust**
3. **The Rust binary goes months without needing updates**
4. **Hot reload is fast enough to feel instant (<100ms)**
5. **No keyring prompts during normal operation**
