//! Per-plugin Lua worker primitive.
//!
//! Exposes a small synchronous bridge used by `lib.plugin_supervisor`.
//! Each plugin key owns one worker thread and one Lua VM. The hub VM keeps
//! routing/descriptor state; plugin-owned handler execution happens here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::lua::primitives::json::json_to_lua;
use crate::lua::LuaRuntime;
use crate::worker::plugin::PLUGIN_WORKER_QUEUE;

const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
pub(crate) struct PluginWorkerRegistry {
    workers: Arc<Mutex<HashMap<String, PluginWorkerHandle>>>,
}

#[derive(Clone)]
struct PluginWorkerHandle {
    tx: mpsc::SyncSender<PluginWorkerRequest>,
}

enum PluginWorkerRequest {
    Invoke {
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        timeout_ms: u64,
        response: mpsc::Sender<Result<serde_json::Value, String>>,
    },
    Shutdown {
        reason: String,
    },
}

#[derive(Debug, Clone)]
struct WorkerSpec {
    plugin_key: String,
    display_name: String,
    init_path: PathBuf,
    source: Option<String>,
    repo_root: Option<PathBuf>,
    lua_base_path: Option<PathBuf>,
}

#[must_use]
pub(crate) fn new_plugin_worker_registry() -> PluginWorkerRegistry {
    PluginWorkerRegistry::default()
}

pub(crate) fn register(lua: &Lua, registry: PluginWorkerRegistry) -> Result<()> {
    let load_registry = registry.clone();
    let load_fn = lua
        .create_function(move |_, spec: Table| {
            let worker_spec = table_to_spec(spec)?;
            load_registry
                .load(worker_spec)
                .map_err(mlua::Error::external)?;
            Ok(true)
        })
        .map_err(|e| anyhow!("create __plugin_worker_load: {e}"))?;
    lua.globals()
        .set("__plugin_worker_load", load_fn)
        .map_err(|e| anyhow!("set __plugin_worker_load: {e}"))?;

    let invoke_registry = registry.clone();
    let invoke_fn = lua
        .create_function(
            move |lua,
                  (plugin_key, kind, id, name, payload, timeout_ms): (
                String,
                String,
                String,
                Option<String>,
                Value,
                u64,
            )| {
                let payload_json: serde_json::Value = lua.from_value(payload).map_err(|e| {
                    mlua::Error::external(format!("plugin worker payload conversion failed: {e}"))
                })?;
                let result = invoke_registry
                    .invoke(&plugin_key, kind, id, name, payload_json, timeout_ms)
                    .map_err(mlua::Error::external)?;
                json_to_lua(lua, &result)
            },
        )
        .map_err(|e| anyhow!("create __plugin_worker_invoke: {e}"))?;
    lua.globals()
        .set("__plugin_worker_invoke", invoke_fn)
        .map_err(|e| anyhow!("set __plugin_worker_invoke: {e}"))?;

    let shutdown_registry = registry.clone();
    let shutdown_fn = lua
        .create_function(move |_, (plugin_key, reason): (String, Option<String>)| {
            shutdown_registry.shutdown(&plugin_key, reason.as_deref().unwrap_or("shutdown"));
            Ok(true)
        })
        .map_err(|e| anyhow!("create __plugin_worker_shutdown: {e}"))?;
    lua.globals()
        .set("__plugin_worker_shutdown", shutdown_fn)
        .map_err(|e| anyhow!("set __plugin_worker_shutdown: {e}"))?;

    Ok(())
}

fn table_to_spec(table: Table) -> mlua::Result<WorkerSpec> {
    let plugin_key: String = table.get("plugin_key")?;
    let display_name: String = table
        .get("display_name")
        .unwrap_or_else(|_| plugin_key.clone());
    let init_path: String = table.get("init_path")?;
    let source: Option<String> = table.get("source").ok();
    let repo_root: Option<String> = table.get("repo_root").ok();
    let lua_base_path: Option<String> = table.get("lua_base_path").ok();
    Ok(WorkerSpec {
        plugin_key,
        display_name,
        init_path: PathBuf::from(init_path),
        source,
        repo_root: repo_root.map(PathBuf::from),
        lua_base_path: lua_base_path.map(PathBuf::from),
    })
}

impl PluginWorkerRegistry {
    fn load(&self, spec: WorkerSpec) -> Result<()> {
        self.shutdown(&spec.plugin_key, "replace");

        let (tx, rx) = mpsc::sync_channel(PLUGIN_WORKER_QUEUE.capacity);
        let (ready_tx, ready_rx) = mpsc::channel();
        let plugin_key = spec.plugin_key.clone();
        thread::Builder::new()
            .name(format!("botster-plugin-{plugin_key}"))
            .spawn(move || worker_loop(spec, rx, ready_tx))
            .map_err(|e| anyhow!("spawn plugin worker: {e}"))?;

        match ready_rx.recv_timeout(LOAD_TIMEOUT) {
            Ok(Ok(())) => {
                self.workers
                    .lock()
                    .expect("PluginWorkerRegistry mutex poisoned")
                    .insert(plugin_key, PluginWorkerHandle { tx });
                Ok(())
            }
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => Err(anyhow!("plugin worker load timeout")),
        }
    }

    fn invoke(
        &self,
        plugin_key: &str,
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value> {
        let handle = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .get(plugin_key)
            .cloned()
            .ok_or_else(|| anyhow!("plugin worker not loaded: {plugin_key}"))?;
        let (response_tx, response_rx) = mpsc::channel();
        handle
            .tx
            .try_send(PluginWorkerRequest::Invoke {
                kind,
                id,
                name,
                payload,
                timeout_ms,
                response: response_tx,
            })
            .map_err(|e| anyhow!("plugin worker queue rejected invoke: {e}"))?;
        match response_rx.recv_timeout(Duration::from_millis(timeout_ms.max(1))) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => Err(anyhow!("plugin worker invoke timeout")),
        }
    }

    fn shutdown(&self, plugin_key: &str, reason: &str) {
        let handle = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .remove(plugin_key);
        if let Some(handle) = handle {
            let _ = handle.tx.try_send(PluginWorkerRequest::Shutdown {
                reason: reason.to_string(),
            });
        }
    }
}

fn worker_loop(
    spec: WorkerSpec,
    rx: mpsc::Receiver<PluginWorkerRequest>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    let mut runtime = match LuaRuntime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("create plugin worker runtime: {err}")));
            return;
        }
    };
    if let Some(lua_base_path) = &spec.lua_base_path {
        runtime.set_base_path(lua_base_path.clone());
        if let Err(err) = runtime.update_package_path(lua_base_path) {
            let _ = ready_tx.send(Err(format!("configure plugin worker lua path: {err}")));
            return;
        }
    } else {
        if let Err(err) = runtime.install_embedded_searcher() {
            let _ = ready_tx.send(Err(format!(
                "install plugin worker embedded modules: {err}"
            )));
            return;
        }
    }

    if let Err(err) = install_worker_bootstrap(&runtime, &spec) {
        let _ = ready_tx.send(Err(err));
        return;
    }

    if let Err(err) = load_plugin_into_worker(&runtime, &spec) {
        let _ = ready_tx.send(Err(err));
        return;
    }
    let _ = ready_tx.send(Ok(()));

    loop {
        let _ = runtime.poll_http_responses();
        let _ = runtime.poll_websocket_events();
        let _ = runtime.poll_user_file_watches();
        let request = match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match request {
            PluginWorkerRequest::Invoke {
                kind,
                id,
                name,
                payload,
                timeout_ms,
                response,
            } => {
                let result =
                    invoke_in_worker(&runtime, &kind, &id, name.as_deref(), payload, timeout_ms);
                let _ = response.send(result);
            }
            PluginWorkerRequest::Shutdown { reason } => {
                log::debug!(
                    "plugin worker {} shutting down: {}",
                    spec.plugin_key,
                    reason
                );
                break;
            }
        }
    }
}

fn install_worker_bootstrap(runtime: &LuaRuntime, spec: &WorkerSpec) -> Result<(), String> {
    let lua = runtime.lua();
    let globals = lua.globals();
    globals
        .set("_loading_plugin_worker", true)
        .map_err(|e| e.to_string())?;
    globals
        .set("_plugin_worker_key", spec.plugin_key.clone())
        .map_err(|e| e.to_string())?;
    if let Some(repo_root) = &spec.repo_root {
        globals
            .set(
                "_loading_plugin_repo_root",
                repo_root.to_string_lossy().to_string(),
            )
            .map_err(|e| e.to_string())?;
    }

    let hub = lua.create_table().map_err(|e| e.to_string())?;
    let hub_id = format!("plugin-worker:{}", spec.plugin_key);
    let hub_id_fn = {
        let hub_id = hub_id.clone();
        lua.create_function(move |_, ()| Ok(hub_id.clone()))
            .map_err(|e| e.to_string())?
    };
    hub.set("hub_id", hub_id_fn).map_err(|e| e.to_string())?;
    let server_id_fn = {
        let hub_id = hub_id.clone();
        lua.create_function(move |_, ()| Ok(hub_id.clone()))
            .map_err(|e| e.to_string())?
    };
    hub.set("server_id", server_id_fn)
        .map_err(|e| e.to_string())?;
    let repo_root = spec.repo_root.clone();
    let detect_repo_fn = lua
        .create_function(move |lua, path: Option<String>| {
            if let Ok(repo) = std::env::var("BOTSTER_REPO") {
                return Ok(Some(repo));
            }

            let detect_path = path
                .map(PathBuf::from)
                .or_else(|| {
                    lua.globals()
                        .get::<Option<String>>("_loading_plugin_repo_root")
                        .ok()
                        .flatten()
                        .map(PathBuf::from)
                })
                .or_else(|| repo_root.clone());

            if let Some(path) = detect_path {
                return Ok(crate::git::repo_name_for_path(&path).ok());
            }

            Ok(None)
        })
        .map_err(|e| e.to_string())?;
    hub.set("detect_repo", detect_repo_fn)
        .map_err(|e| e.to_string())?;
    hub.set(
        "get_worktrees",
        lua.create_function(|lua, ()| lua.create_table())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    globals.set("hub", hub).map_err(|e| e.to_string())?;

    lua.load(
        r#"
        local state = require("hub.state")
        local hooks = require("hub.hooks")
        local loader = require("hub.loader")

        _G.state = state
        _G.hooks = hooks
        _G.loader = loader

        local function safe_require(module_name)
            local ok, result = pcall(require, module_name)
            if ok then return result end
            if log and log.debug then
                log.debug(string.format("plugin worker skipped %s: %s", module_name, tostring(result)))
            end
            return nil
        end

        safe_require("lib.config_resolver")
        safe_require("lib.commands")
        safe_require("lib.session_actions")
        _G.action = safe_require("lib.action")
        _G.surfaces = safe_require("lib.surfaces")
        safe_require("lib.plugin_assets")
        safe_require("lib.entity_broadcast")
        _G.mcp = safe_require("lib.mcp")

        local plugin_db = safe_require("lib.plugin_db")
        if plugin_db and type(plugin_db.install) == "function" then
            plugin_db.install()
        end
        "#,
    )
    .set_name("plugin_worker_bootstrap")
    .exec()
    .map_err(|e| e.to_string())
}

fn load_plugin_into_worker(runtime: &LuaRuntime, spec: &WorkerSpec) -> Result<(), String> {
    let lua = runtime.lua();
    let repo_root = spec
        .repo_root
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    let source = spec.source.clone();
    let globals = lua.globals();
    globals
        .set(
            "__plugin_init_path",
            spec.init_path.to_string_lossy().to_string(),
        )
        .map_err(|e| e.to_string())?;
    globals
        .set("__plugin_display_name", spec.display_name.clone())
        .map_err(|e| e.to_string())?;
    globals
        .set("__plugin_source", source)
        .map_err(|e| e.to_string())?;
    globals
        .set("__plugin_repo_root", repo_root)
        .map_err(|e| e.to_string())?;
    lua.load(
        r#"
        local loader = require("hub.loader")
        local ok, err = loader.load_plugin(__plugin_init_path, __plugin_display_name, {
            source = __plugin_source,
            repo_root = __plugin_repo_root,
        })
        if not ok then error(err or "plugin worker load failed") end
        "#,
    )
    .set_name("plugin_worker_load")
    .exec()
    .map_err(|e| e.to_string())
}

fn invoke_in_worker(
    runtime: &LuaRuntime,
    kind: &str,
    id: &str,
    name: Option<&str>,
    payload: serde_json::Value,
    timeout_ms: u64,
) -> Result<serde_json::Value, String> {
    let lua = runtime.lua();
    let payload_lua = json_to_lua(lua, &payload).map_err(|e| e.to_string())?;
    let globals = lua.globals();
    globals.set("__handler_id", id).map_err(|e| e.to_string())?;
    globals
        .set("__handler_name", name)
        .map_err(|e| e.to_string())?;
    globals
        .set("__handler_timeout_ms", timeout_ms)
        .map_err(|e| e.to_string())?;
    globals
        .set("__payload", payload_lua)
        .map_err(|e| e.to_string())?;
    let result: Value = match kind {
        "ui_action" => lua
            .load(
                r#"
                local action = require("lib.action")
                local ok, result = __hook_timed_pcall(function()
                    return action._invoke_registered(__handler_id, __handler_name, __payload.envelope, __payload.ctx)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_ui_action")
            .eval()
            .map_err(|e| e.to_string())?,
        "session_action" => lua
            .load(
                r#"
                local actions = require("lib.session_actions")
                local ok, result = __hook_timed_pcall(function()
                    return actions._invoke_registered(__handler_id, __payload.session_uuid, __payload.payload)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_session_action")
            .eval()
            .map_err(|e| e.to_string())?,
        "command" => lua
            .load(
                r#"
                local commands = require("lib.commands")
                local ok, result = __hook_timed_pcall(function()
                    return commands._invoke_registered(__handler_id, __payload.command)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_command")
            .eval()
            .map_err(|e| e.to_string())?,
        "hook_observer" => lua
            .load(
                r#"
                local hooks = require("hub.hooks")
                local ok, result = __hook_timed_pcall(function()
                    return hooks._invoke_observer(__handler_id, __handler_name, __payload.args)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_hook_observer")
            .eval()
            .map_err(|e| e.to_string())?,
        "hook_interceptor" => lua
            .load(
                r#"
                local hooks = require("hub.hooks")
                local ok, result = __hook_timed_pcall(function()
                    return hooks._invoke_interceptor(__handler_id, __handler_name, __payload.args)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_hook_interceptor")
            .eval()
            .map_err(|e| e.to_string())?,
        "surface_route" => lua
            .load(
                r#"
                local surfaces = require("lib.surfaces")
                local ok, result = __hook_timed_pcall(function()
                    return surfaces._invoke_render(__handler_id, __payload.render_state)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_surface_route")
            .eval()
            .map_err(|e| e.to_string())?,
        "asset_message" => lua
            .load(
                r#"
                local plugin_assets = require("lib.plugin_assets")
                local ok, result = __hook_timed_pcall(function()
                    return plugin_assets._invoke_message(
                        __handler_name,
                        __handler_id,
                        __payload.message,
                        __payload.ctx
                    )
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_asset_message")
            .eval()
            .map_err(|e| e.to_string())?,
        "timer" => lua
            .load(
                r#"
                local ok, result = __hook_timed_pcall(function()
                    return timer._invoke_registered(__handler_id)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_timer")
            .eval()
            .map_err(|e| e.to_string())?,
        "event" => lua
            .load(
                r#"
                local ok, result = __hook_timed_pcall(function()
                    return events._invoke_registered(__handler_id, __payload.data)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_event")
            .eval()
            .map_err(|e| e.to_string())?,
        "watch" => lua
            .load(
                r#"
                local ok, result = __hook_timed_pcall(function()
                    return watch._invoke_registered(__handler_id, __payload.event)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_watch")
            .eval()
            .map_err(|e| e.to_string())?,
        "mcp_tool" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                local ok, result = __hook_timed_pcall(function()
                    return mcp._invoke_tool(__handler_id, __payload.params, __payload.context)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_tool")
            .eval()
            .map_err(|e| e.to_string())?,
        "mcp_prompt" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                local ok, result = __hook_timed_pcall(function()
                    return mcp._invoke_prompt(__handler_id, __payload.args)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_prompt")
            .eval()
            .map_err(|e| e.to_string())?,
        "mcp_resource" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                local ok, result = __hook_timed_pcall(function()
                    return mcp._invoke_resource(__handler_id, __payload.params, __payload.context)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_resource")
            .eval()
            .map_err(|e| e.to_string())?,
        "mcp_proxy_auth_error" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                local ok, result = __hook_timed_pcall(function()
                    return mcp._invoke_proxy_auth_error(__handler_id)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_proxy_auth_error")
            .eval()
            .map_err(|e| e.to_string())?,
        _ => return Err(format!("unsupported plugin handler kind: {kind}")),
    };

    lua.from_value(result)
        .map_err(|e| format!("plugin worker result conversion failed: {e}"))
}
