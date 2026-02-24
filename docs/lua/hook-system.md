# Botster Hook System and Event Catalog

The hook system (`hub/hooks.lua`) provides two distinct hook types with priority queues and enable/disable fast paths. Both are protected from hot-reload — the registry survives module reloads.

## Hook API

### Observers (fire-and-forget, async-safe)

```lua
hooks.on(event, name, callback, { priority = 100, enabled = true })
hooks.off(event, name)
hooks.notify(event, ...)        -- returns count of callbacks fired
hooks.has_observers(event)      -- fast-path check
hooks.enable(event, name)
hooks.disable(event, name)
```

### Interceptors (synchronous, blocking, can transform or drop)

```lua
hooks.intercept(event, name, callback, { priority = 100, timeout_ms = 10 })
hooks.unintercept(event, name)
hooks.call(event, ...)          -- returns transformed value, or nil to block
hooks.has_interceptors(event)   -- fast-path check
```

Interceptors run in priority order. Each receives the (possibly transformed) output of the previous. Returning nil blocks the action entirely.

## Observer Events

| Event | Registered in | Fires when |
|-------|---------------|------------|
| `agent_created` | `connections.lua` | Agent spawned, broadcasts to all clients |
| `agent_deleted` | `connections.lua` | Agent removed, broadcasts to all clients |
| `agent_lifecycle` | `connections.lua` | Lifecycle stage changes (creating_worktree, etc.) |
| `_pty_notification_raw` | `connections.lua` | Internal: enriches raw notification with focus/index |
| `pty_notification` | `connections.lua` | Sends web push notification |
| `pty_title_changed` | `connections.lua` | OSC 0/2 title change -> updates agent.title, broadcasts |
| `pty_cwd_changed` | `connections.lua` | OSC 7 cwd change -> updates agent.cwd, broadcasts |
| `pty_prompt` | `connections.lua` | OSC 133/633 prompt marks |
| `pty_input` | `connections.lua` | Notification cleared by user typing |
| `client_connected` | (user hook point) | Client joined the registry |
| `client_disconnected` | (user hook point) | Client left the registry |
| `after_agent_create` | `lib/agent.lua` | After Agent.new() completes |
| `before_agent_close` | `lib/agent.lua` | Before sessions are killed |
| `after_agent_close` | `lib/agent.lua` | After agent is removed |
| `shutdown` | `hub/init.lua` | Hub shutting down |

## Interceptor Events

| Event | Called by | Can do |
|-------|-----------|--------|
| `before_agent_create` | `handlers/agents.lua` | Transform params or return nil to block creation |
| `before_agent_delete` | `handlers/agents.lua` | Transform params or return nil to block deletion |
| `before_client_subscribe` | `lib/client.lua` | Transform or block subscription requests |
| `filter_agent_env` | `lib/agent.lua` | Modify environment variables for PTY sessions |

## Events System (Rust -> Lua)

The `events` primitive provides a separate pub-sub layer from hooks. Rust emits events via `LuaRuntime::emit_event()`, Lua listens via `events.on()`.

| Event | Source | Payload |
|-------|--------|---------|
| `command_message` | `hub_commands.lua` via ActionCable | create_agent / delete_agent commands |
| `worktree_created` | Rust async worktree create | `{branch, path, issue_number, prompt, agent_key, profile_name, client_rows, client_cols}` |
| `worktree_create_failed` | Rust async worktree create | `{branch, error}` |
| `connection_code_ready` | Rust connection generation | `{url, qr_ascii}` |
| `connection_code_error` | Rust connection generation | error string |
| `agent_status_changed` | Rust/Lua | `{agent_id, status}` |
| `process_exited` | Rust PTY watcher | `{agent_key, exit_code}` |
| `outgoing_signal` | Rust WebRTC | Pre-encrypted signal data for ActionCable relay |

## Rust -> Lua Bridge Methods

These are `LuaRuntime` methods called from the Rust event loop that invoke Lua callbacks:

| Rust method | Triggers | On event |
|-------------|----------|----------|
| `call_peer_connected(peer_id)` | `webrtc.on_peer_connected` | `HubEvent::DcOpened` |
| `call_peer_disconnected(peer_id)` | `webrtc.on_peer_disconnected` | WebRTC disconnect |
| `call_webrtc_message(peer_id, msg)` | `webrtc.on_message` | `HubEvent::WebRtcMessage` |
| `notify_pty_notification(info)` | `hooks.notify("_pty_notification_raw", ...)` | `HubEvent::PtyNotification` |
| `notify_pty_osc_event(...)` | `hooks.notify("pty_title_changed"/"pty_cwd_changed"/"pty_prompt")` | `HubEvent::PtyOscEvent` |
| `notify_pty_input(agent_index)` | `_on_pty_input(agent_index)` global | PTY input on notification-active agents |
| `emit_event(event, data)` | `events.emit(event, data)` | Various `HubEvent` variants |
| `notify_tui_connected()` | `tui.on_connected` | TUI connect |
| `notify_tui_disconnected()` | `tui.on_disconnected` | TUI disconnect |
| `call_tui_message(msg)` | `tui.on_message` | TUI message |
| `call_socket_client_connected(id)` | `socket.on_client_connected` | `HubEvent::SocketClientConnected` |
| `call_socket_client_disconnected(id)` | `socket.on_client_disconnected` | `HubEvent::SocketClientDisconnected` |
| `call_socket_message(id, msg)` | `socket.on_message` | `HubEvent::SocketMessage` |
| `fire_timer(id)` | Timer callback | `HubEvent::TimerFired` |
| `fire_user_file_watch(id, events)` | `watch.directory` callback | `HubEvent::UserFileWatch` |
| `fire_http_response(resp)` | `http.request` callback | `HubEvent::HttpResponse` |
| `fire_ac_message(channel_id, msg)` | ActionCable callback | `HubEvent::AcChannelMessage` |
| `reload_lua_modules(modules)` | `loader.reload(name)` | `HubEvent::LuaFileChange` |

## Global Functions (Rust-set, Lua-callable)

| Function | Purpose |
|----------|---------|
| `_on_pty_input(agent_index)` | PTY input hot path, clears notifications |
| `_clear_agent_notification(agent_index)` | Explicit notification clear |
| `_set_pty_focused(agent_index, pty_index, peer_id, focused)` | Focus state tracking |
