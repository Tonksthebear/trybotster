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
    config_resolver.lua  # 2-layer .botster/ config resolution
    mcp.lua          # MCP tool registry (plugins register tools here)
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
    plugin_watcher.lua # Hot-reload via PollWatcher for plugin directories
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
  plugins/*/init.lua         # Plugin entrypoints (loaded by ConfigResolver)
  improvements/*.lua         # Agent-written improvements (sandboxed)
```

Override search chain for `require()`: `{repo}/.botster/lua/` -> `~/.botster/lua/` -> embedded binary (fallback). The `package.path` includes `?.lua`, `?/init.lua`, `lib/?.lua`, `handlers/?.lua`, and `hub/?.lua`. While a plugin loads, Botster also adds that plugin's root directory and optional `lua/` directory so `init.lua` can require sibling files.

## `.botster/` Config Directory (2-Layer Resolution)

ConfigResolver merges configs across 2 layers (repo wins on name collision):

```
~/.botster/                              # device_root (Layer 1)
  agents/
    claude/
      initialization                     # REQUIRED: at least one agent config
      notes.md                           # Optional paired file owned by this agent
  accessories/
    rails-server/                        # Optional: plain PTY sessions
      initialization
      port_forward                       # Sentinel file: session gets $PORT env var
  workspaces/
    dev.json                             # Workspace manifest (auto-spawn group)
  plugins/
    {name}/
      init.lua                           # Device-level plugin entrypoint
      web_layout.lua                     # Optional plugin-owned support files
      tui/status.lua                     # Optional TUI file declared by init.lua

{repo}/.botster/                         # repo_root (Layer 2, highest priority)
  agents/ accessories/ workspaces/ plugins/
```

Agents, accessories, and plugins are directory units merged per-name with repo overriding device. Only the entrypoint filename is fixed: sessions use `initialization`, plugins use `init.lua`, and any other files in the directory are owned by that definition. Session scripts can use `botster context session_dir` to locate the owning `agents/<name>/` or `accessories/<name>/` directory, or `botster context file <relative-path>` to read a paired file by whatever name the author chooses.

Worktree file copying and cleanup are handled via the **Workspace Include** plugin template (hooks into `worktree_created` and `worktree_deleted`).

## Runtime Config Paths

| Path | Content |
|------|---------|
| `~/.config/botster/config.json` | Main CLI config (macOS: `~/Library/Application Support/botster/`) |
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
