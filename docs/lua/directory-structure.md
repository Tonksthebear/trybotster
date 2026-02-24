# Botster Directory Structure and Config Resolution

## Source Tree (Embedded in Binary)

```
cli/lua/
  hub/
    init.lua         # Bootstrap entrypoint (loaded explicitly, not hot-reloadable)
    hooks.lua        # Hook system (protected, never reloaded)
    loader.lua       # Module loader + sandbox (protected, never reloaded)
    state.lua        # KV store surviving hot-reload (protected, never reloaded)
  lib/
    agent.lua        # Agent class
    client.lua       # Generic client/subscription interface
    commands.lua     # Command registry
    config_resolver.lua  # 4-layer .botster/ config resolution
    pty_clients.lua  # PTY focus tracking
  handlers/
    agents.lua       # Agent lifecycle orchestration
    connections.lua  # Client registry, notification routing
    commands.lua     # Built-in command registrations
    webrtc.lua       # WebRTC transport handler
    tui.lua          # TUI transport handler
    socket.lua       # Unix socket IPC transport handler
    hub_commands.lua # ActionCable HubCommandChannel plugin
    filesystem.lua   # fs:* browser commands
    templates.lua    # template:* commands
  ui/                # Separate mlua instance (TUI-side)
    layout.lua       # Declarative ratatui layout
    keybindings.lua  # Key descriptor -> action mapping
    actions.lua      # Workflow dispatch
    events.lua       # Hub event handler for TUI
    botster.lua      # Neovim-style extension API
```

In **release builds**, all files are embedded via `build.rs` into `EMBEDDED_LUA_FILES`. A custom `package.searchers` entry resolves `require()` against embedded modules. In **debug builds**, files load from the source tree filesystem, enabling hot-reload during development.

## User Override Hierarchy

```
~/.botster/lua/
  hub/              # Override hub modules
  lib/              # Override library modules
  handlers/         # Override handler modules
  ui/               # Override TUI modules
  user/
    init.lua        # Main user customization entry point (like nvim's init.lua)
    ui/
      layout.lua    # Highest-priority TUI layout override
      keybindings.lua
      actions.lua
  plugins/*/init.lua         # User plugins (loaded by ConfigResolver)
  improvements/*.lua         # Agent-written improvements (sandboxed)
```

Override search chain for `require()`: `{repo}/.botster/lua/` -> `~/.botster/lua/` -> embedded binary (fallback). The `package.path` includes `?.lua`, `?/init.lua`, `lib/?.lua`, `handlers/?.lua`, `hub/?.lua`, `plugins/?.lua`, `plugins/?/init.lua`.

## `.botster/` Config Directory (4-Layer Resolution)

ConfigResolver merges configs across 4 layers (most-specific wins on name collision):

```
~/.botster/                              # device_root
  shared/                                # Layer 1: device shared
    sessions/
      agent/
        initialization                   # REQUIRED: startup script for main agent PTY
      server/                            # Optional: additional named sessions
        initialization
        port_forward                     # Sentinel file: session gets $PORT env var
    plugins/{name}/init.lua              # Device-level plugins
    workspace_include                    # Glob patterns for file copying to worktrees
    workspace_teardown                   # Cleanup script
  profiles/{profile-name}/              # Layer 2: device profile
    sessions/ plugins/ workspace_include/ workspace_teardown  (same structure)

{repo}/.botster/                         # repo_root
  shared/                                # Layer 3: repo shared
    sessions/ plugins/ workspace_include/ workspace_teardown
  profiles/{profile-name}/              # Layer 4: repo profile (highest priority)
    sessions/ plugins/ workspace_include/ workspace_teardown
```

Sessions, plugins, `workspace_include`, and `workspace_teardown` are all merged per-name with higher layers winning.

## Runtime Config Paths

| Path | Content |
|------|---------|
| `~/.config/botster/config.json` | Main CLI config (macOS: `~/Library/Application Support/botster/`) |
| `~/.config/botster/hub_registry.json` | hub_id -> hub display name |
| `~/.config/botster/device.json` | Device identity |
| `~/.config/botster/credentials.json` | API token fallback (0600 perms, when no keyring) |
| `~/.config/botster/hubs/{hub_id}/` | Per-hub encrypted state (OlmCrypto, WebRTC) |

Debug builds use `botster-dev` instead of `botster` as the directory name.

## Config Defaults (from `Config::default()`)

```
server_url: "https://trybotster.com" (release) / "https://dev.trybotster.com" (debug)
poll_interval: 5 (seconds)
agent_timeout: 3600 (seconds)
max_sessions: 20
worktree_base: ~/botster-sessions/
```

## Environment Variables

| Variable | Effect |
|----------|--------|
| `BOTSTER_ENV` | `test`, `system_test`, `development`/`dev`, or unset (production) |
| `BOTSTER_CONFIG_DIR` | Override config directory path |
| `BOTSTER_SERVER_URL` | Override server URL |
| `BOTSTER_TOKEN` | API token (bypasses keyring, for CI/CD) |
| `BOTSTER_WORKTREE_BASE` | Override worktree base directory |
| `BOTSTER_POLL_INTERVAL` | Override poll interval (seconds) |
| `BOTSTER_MAX_SESSIONS` | Override max concurrent sessions |
| `BOTSTER_AGENT_TIMEOUT` | Override agent idle timeout (seconds) |
| `BOTSTER_LUA_PATH` | Override Lua script base path (default: `~/.botster/lua`) |
| `BOTSTER_LUA_STRICT` | If `"1"`, Lua errors panic instead of log |
| `BOTSTER_LOG_FILE` | Override log file path |
| `BOTSTER_REPO` | Repository identifier (`owner/repo`) |
