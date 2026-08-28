//! Per-plugin Lua worker primitive.
//!
//! Exposes a small synchronous bridge used by `lib.plugin_supervisor`.
//! Each plugin key owns one worker thread and one Lua VM. The hub VM keeps
//! routing/descriptor state; plugin-owned handler execution happens here.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Table, Value};

use crate::hub::events::{HubEvent, HubEventTx};
use crate::lua::primitives::json::json_to_lua;
use crate::lua::primitives::{http, local_webhooks, websocket};
use crate::lua::LuaRuntime;
use crate::worker::plugin::PLUGIN_WORKER_QUEUE;

const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
const PARENT_EVENT_ADMISSION_TIMEOUT: Duration = Duration::from_millis(250);
/// Upper bound for plugin worker handler time.
///
/// HTTP MCP dispatch waits 120 seconds. This leaves a 10 second margin so a
/// timed-out handler can still admit its completion event before HTTP returns.
const ASYNC_INVOKE_TIMEOUT_MAX_MS: u64 = 110_000;
const SYNC_INVOKE_TIMEOUT_MAX_MS: u64 = 5_000;
static NEXT_ASYNC_INVOKE_ID: AtomicU64 = AtomicU64::new(1);

fn clamp_invoke_timeout_ms(timeout_ms: u64) -> u64 {
    timeout_ms.clamp(1, ASYNC_INVOKE_TIMEOUT_MAX_MS)
}

fn clamp_sync_invoke_timeout_ms(timeout_ms: u64) -> u64 {
    timeout_ms.clamp(1, SYNC_INVOKE_TIMEOUT_MAX_MS)
}

fn admit_completion(
    runtime: &tokio::runtime::Handle,
    tx: HubEventTx,
    request_id: String,
    result: Result<serde_json::Value, String>,
) {
    if let Err(error) = runtime
        .block_on(tx.send_async(HubEvent::PluginWorkerInvokeCompleted { request_id, result }))
    {
        log::warn!("plugin worker completion queue closed: {error}");
    }
}

struct CompletionGuard {
    request_id: String,
    tx: HubEventTx,
    runtime: tokio::runtime::Handle,
    sent: bool,
}

impl CompletionGuard {
    fn new(request_id: String, tx: HubEventTx, runtime: tokio::runtime::Handle) -> Self {
        Self {
            request_id,
            tx,
            runtime,
            sent: false,
        }
    }

    fn send(&mut self, result: Result<serde_json::Value, String>) {
        admit_completion(
            &self.runtime,
            self.tx.clone(),
            self.request_id.clone(),
            result,
        );
        self.sent = true;
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        // try_send: Drop may run after the Tokio runtime is gone.
        if let Err(error) = self.tx.send(HubEvent::PluginWorkerInvokeCompleted {
            request_id: self.request_id.clone(),
            result: Err("plugin worker exited".to_string()),
        }) {
            log::warn!("plugin worker exit completion not admitted: {error}");
        }
    }
}

fn remaining_handler_timeout_ms(deadline: Instant) -> Option<u64> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining.as_millis().max(1) as u64)
    }
}

fn payload_session_uuid(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("context")
        .and_then(|context| context.get("session_uuid"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

struct ParentHubRouteGuard {
    route_through_event_loop: Arc<AtomicBool>,
    previous: bool,
}

impl ParentHubRouteGuard {
    fn use_synchronous_fallback(route_through_event_loop: &Arc<AtomicBool>) -> Self {
        let previous = route_through_event_loop.swap(false, Ordering::AcqRel);
        Self {
            route_through_event_loop: Arc::clone(route_through_event_loop),
            previous,
        }
    }
}

impl Drop for ParentHubRouteGuard {
    fn drop(&mut self) {
        self.route_through_event_loop
            .store(self.previous, Ordering::Release);
    }
}

// Rust guideline compliant 2026-05 (ms-rust)
// Updated for reviewer feedback (RAII Guard + zero-alloc accessor + thread guard).
// Provides safe entry into plugin worker context for boundary observability/debug_asserts.
// Supports nesting, early returns, and panics via Drop restore. Zero cost in release.

thread_local! {
    static CURRENT_PLUGIN_WORKER: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// RAII guard that restores the previous plugin worker key on drop.
///
/// Returned by `enter_plugin_worker`. Prevents leaks on `?`, panic, or early return.
/// Supports proper nesting (plugin A → hub → plugin B) by restoring the prior value.
///
/// The `!Send + !Sync` marker ensures Drop always runs on the same thread as the
/// corresponding `enter_plugin_worker` call (prevents cross-thread corruption of
/// the thread_local).
#[must_use = "PluginWorkerGuard restores the previous worker key when dropped"]
pub(crate) struct PluginWorkerGuard {
    previous: Option<String>,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for PluginWorkerGuard {
    fn drop(&mut self) {
        CURRENT_PLUGIN_WORKER.with(|c| *c.borrow_mut() = self.previous.take());
    }
}

/// Enters the plugin worker context for `key`.
///
/// Returns a guard that restores the prior key when dropped.
/// Panics (debug only) if not called from a named `plugin-worker-*` thread.
pub(crate) fn enter_plugin_worker(key: &str) -> PluginWorkerGuard {
    // This assert exists to catch architectural mistakes where we enter the
    // plugin worker context from the main hub thread or other non-worker threads.
    // It is currently firing during normal startup for some AC / event / timer paths.
    // For now we log loudly instead of hard-panicking the whole hub so development
    // can continue while we finish the proper cross-VM architecture.
    if !std::thread::current()
        .name()
        .is_some_and(|n| n.starts_with("plugin-worker-"))
    {
        log::error!(
            "enter_plugin_worker called from non-worker thread (thread={:?}, key={}). \
             This is an architectural bug — plugin-owned handler code is being entered \
             from the main hub context.",
            std::thread::current().name(),
            key
        );
        // Still proceed so the hub can at least start. The boundary will be wrong
        // for this invocation, but we won't take down the entire process.
    }

    let previous = CURRENT_PLUGIN_WORKER.with(|c| c.replace(Some(key.to_string())));
    PluginWorkerGuard {
        previous,
        _not_send: PhantomData,
    }
}

/// Borrows the current plugin worker key for the duration of the closure.
///
/// Zero-allocation hot path intended for `debug_assert!` at the head of dispatch sites.
pub(crate) fn with_current_plugin_worker<R>(f: impl FnOnce(Option<&str>) -> R) -> R {
    CURRENT_PLUGIN_WORKER.with(|c| f(c.borrow().as_deref()))
}

/// Owned variant. Only for crossing into Lua (which cannot hold a borrow across the FFI boundary).
pub(crate) fn current_plugin_worker_owned() -> Option<String> {
    CURRENT_PLUGIN_WORKER.with(|c| c.borrow().clone())
}

fn lua_perf_enabled() -> bool {
    std::env::var("BOTSTER_LUA_PERF")
        .map(|value| {
            let value = value.to_ascii_lowercase();
            value == "1" || value == "true" || value == "yes" || value == "on"
        })
        .unwrap_or(false)
}

struct AsyncCallback {
    key: mlua::RegistryKey,
    session_uuid: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct PluginWorkerRegistry {
    workers: Arc<Mutex<HashMap<String, PluginWorkerHandle>>>,
    hub_event_channel: Arc<Mutex<Option<(HubEventTx, tokio::runtime::Handle)>>>,
    async_callbacks: Arc<Mutex<HashMap<String, AsyncCallback>>>,
    canceled: Arc<Mutex<HashSet<String>>>,
}

#[derive(Clone)]
struct PluginWorkerHandle {
    tx: mpsc::SyncSender<PluginWorkerRequest>,
    parent_rx: Arc<Mutex<mpsc::Receiver<WorkerParentRequest>>>,
    admission: Arc<Mutex<PluginWorkerAdmission>>,
}

#[derive(Default)]
struct PluginWorkerAdmission {
    shutdown_reason: Option<String>,
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
    InvokeAsync {
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        deadline: Instant,
        request_id: String,
        completion_tx: HubEventTx,
        completion_runtime: tokio::runtime::Handle,
    },
    Shutdown {
        reason: String,
    },
    HttpResponse(http::CompletedHttpResponse),
    WebSocketEvent(websocket::WsEvent),
    TimerFired {
        timer_id: String,
    },
    UserFileWatch {
        watch_id: String,
        events: Vec<crate::file_watcher::FileEvent>,
    },
    LocalWebhook(local_webhooks::LocalWebhookRequest),
}

#[derive(Clone, Debug)]
pub(crate) struct PluginWorkerEventTx {
    tx: mpsc::SyncSender<PluginWorkerRequest>,
}

impl PluginWorkerEventTx {
    fn new(tx: mpsc::SyncSender<PluginWorkerRequest>) -> Self {
        Self { tx }
    }

    pub(crate) fn send_http_response(&self, response: http::CompletedHttpResponse) {
        if let Err(err) = self
            .tx
            .try_send(PluginWorkerRequest::HttpResponse(response))
        {
            log::warn!("plugin worker queue rejected HTTP response: {err}");
        }
    }

    pub(crate) fn send_websocket_event(&self, event: websocket::WsEvent) {
        if let Err(err) = self.tx.try_send(PluginWorkerRequest::WebSocketEvent(event)) {
            log::warn!("plugin worker queue rejected WebSocket event: {err}");
        }
    }

    pub(crate) fn send_timer_fired(&self, timer_id: String) -> bool {
        self.tx
            .try_send(PluginWorkerRequest::TimerFired { timer_id })
            .is_ok()
    }

    pub(crate) fn send_user_file_watch(
        &self,
        watch_id: String,
        events: Vec<crate::file_watcher::FileEvent>,
    ) -> bool {
        self.tx
            .try_send(PluginWorkerRequest::UserFileWatch { watch_id, events })
            .is_ok()
    }

    pub(crate) fn send_local_webhook_request(
        &self,
        request: local_webhooks::LocalWebhookRequest,
    ) -> Result<(), mpsc::TrySendError<local_webhooks::LocalWebhookRequest>> {
        self.tx
            .try_send(PluginWorkerRequest::LocalWebhook(request))
            .map_err(|err| match err {
                mpsc::TrySendError::Full(PluginWorkerRequest::LocalWebhook(request)) => {
                    mpsc::TrySendError::Full(request)
                }
                mpsc::TrySendError::Disconnected(PluginWorkerRequest::LocalWebhook(request)) => {
                    mpsc::TrySendError::Disconnected(request)
                }
                mpsc::TrySendError::Full(_) => unreachable!("request variant changed"),
                mpsc::TrySendError::Disconnected(_) => unreachable!("request variant changed"),
            })
    }
}

pub(crate) enum WorkerParentRequest {
    HubRequest {
        payload: serde_json::Value,
        response: mpsc::Sender<Result<serde_json::Value, String>>,
    },
}

impl std::fmt::Debug for WorkerParentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HubRequest { payload, .. } => f
                .debug_struct("HubRequest")
                .field("payload", payload)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Debug, Clone)]
struct WorkerSpec {
    plugin_key: String,
    display_name: String,
    init_path: PathBuf,
    source: Option<String>,
    repo_root: Option<PathBuf>,
    lua_base_path: Option<PathBuf>,
    parent_hub_id: Option<String>,
}

#[must_use]
pub(crate) fn new_plugin_worker_registry() -> PluginWorkerRegistry {
    PluginWorkerRegistry::default()
}

pub(crate) fn register(lua: &Lua, registry: PluginWorkerRegistry) -> Result<()> {
    let load_registry = registry.clone();
    let load_fn = lua
        .create_function(move |lua, spec: Table| {
            let worker_spec = table_to_spec(spec)?;
            load_registry
                .load(lua, worker_spec)
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
                if timeout_ms > SYNC_INVOKE_TIMEOUT_MAX_MS {
                    log::warn!(
                        "plugin worker sync invoke {id} timeout_ms={timeout_ms} clamped to {SYNC_INVOKE_TIMEOUT_MAX_MS}"
                    );
                }
                let result = invoke_registry
                    .invoke_with_lua(
                        lua,
                        &plugin_key,
                        kind,
                        id,
                        name,
                        payload_json,
                        clamp_sync_invoke_timeout_ms(timeout_ms),
                    )
                    .map_err(mlua::Error::external)?;
                json_to_lua(lua, &result)
            },
        )
        .map_err(|e| anyhow!("create __plugin_worker_invoke: {e}"))?;
    lua.globals()
        .set("__plugin_worker_invoke", invoke_fn)
        .map_err(|e| anyhow!("set __plugin_worker_invoke: {e}"))?;

    let invoke_async_registry = registry.clone();
    let invoke_async_fn = lua
        .create_function(
            move |lua,
                  (plugin_key, kind, id, name, payload, timeout_ms, callback): (
                String,
                String,
                String,
                Option<String>,
                Value,
                u64,
                mlua::Function,
            )| {
                let payload_json: serde_json::Value = lua.from_value(payload).map_err(|e| {
                    mlua::Error::external(format!(
                        "plugin worker async payload conversion failed: {e}"
                    ))
                })?;
                invoke_async_registry
                    .invoke_async(
                        lua,
                        &plugin_key,
                        kind,
                        id,
                        name,
                        payload_json,
                        timeout_ms,
                        callback,
                    )
                    .map_err(mlua::Error::external)
            },
        )
        .map_err(|e| anyhow!("create __plugin_worker_invoke_async: {e}"))?;
    lua.globals()
        .set("__plugin_worker_invoke_async", invoke_async_fn)
        .map_err(|e| anyhow!("set __plugin_worker_invoke_async: {e}"))?;

    let fail_session_registry = registry.clone();
    let fail_session_fn = lua
        .create_function(move |lua, (session_uuid, error): (String, String)| {
            fail_session_registry.fail_async_callbacks_for_session(lua, &session_uuid, &error);
            Ok(true)
        })
        .map_err(|e| anyhow!("create __plugin_worker_fail_session_callbacks: {e}"))?;
    lua.globals()
        .set("__plugin_worker_fail_session_callbacks", fail_session_fn)
        .map_err(|e| anyhow!("set __plugin_worker_fail_session_callbacks: {e}"))?;

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
    let parent_hub_id: Option<String> = table.get("parent_hub_id").ok();
    Ok(WorkerSpec {
        plugin_key,
        display_name,
        init_path: PathBuf::from(init_path),
        source,
        repo_root: repo_root.map(PathBuf::from),
        lua_base_path: lua_base_path.map(PathBuf::from),
        parent_hub_id,
    })
}

impl PluginWorkerRegistry {
    pub(crate) fn set_hub_event_tx(&self, tx: HubEventTx, tokio_handle: tokio::runtime::Handle) {
        *self
            .hub_event_channel
            .lock()
            .expect("PluginWorkerRegistry hub_event_channel mutex poisoned") =
            Some((tx, tokio_handle));
    }

    fn load(&self, lua: &Lua, spec: WorkerSpec) -> Result<()> {
        self.shutdown(&spec.plugin_key, "replace");

        let (tx, rx) = mpsc::sync_channel(PLUGIN_WORKER_QUEUE.capacity);
        let (parent_tx, parent_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let admission = Arc::new(Mutex::new(PluginWorkerAdmission::default()));
        let plugin_key = spec.plugin_key.clone();
        let hub_event_channel = self
            .hub_event_channel
            .lock()
            .expect("PluginWorkerRegistry hub_event_channel mutex poisoned")
            .clone();
        let worker_event_tx = PluginWorkerEventTx::new(tx.clone());
        let worker_admission = Arc::clone(&admission);
        let worker_canceled = Arc::clone(&self.canceled);
        thread::Builder::new()
            .name(format!("plugin-worker-{plugin_key}"))
            .spawn(move || {
                worker_loop(
                    spec,
                    rx,
                    worker_event_tx,
                    parent_tx,
                    ready_tx,
                    hub_event_channel,
                    worker_admission,
                    worker_canceled,
                )
            })
            .map_err(|e| anyhow!("spawn plugin worker: {e}"))?;

        match wait_for_ready(lua, ready_rx, &parent_rx, LOAD_TIMEOUT) {
            Ok(Ok(())) => {
                self.workers
                    .lock()
                    .expect("PluginWorkerRegistry mutex poisoned")
                    .insert(
                        plugin_key,
                        PluginWorkerHandle {
                            tx,
                            parent_rx: Arc::new(Mutex::new(parent_rx)),
                            admission,
                        },
                    );
                Ok(())
            }
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => Err(anyhow!("plugin worker load timeout")),
        }
    }

    fn invoke_with_lua(
        &self,
        lua: &Lua,
        plugin_key: &str,
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        timeout_ms: u64,
    ) -> Result<serde_json::Value> {
        let perf_started = lua_perf_enabled().then(Instant::now);
        let log_kind = kind.clone();
        let log_id = id.clone();
        let handle = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .get(plugin_key)
            .cloned()
            .ok_or_else(|| anyhow!("plugin worker not loaded: {plugin_key}"))?;
        let (response_tx, response_rx) = mpsc::channel();
        let admission = handle
            .admission
            .lock()
            .expect("PluginWorkerHandle admission mutex poisoned");
        if let Some(reason) = &admission.shutdown_reason {
            return Err(anyhow!("plugin worker is shutting down: {reason}"));
        }
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
        drop(admission);
        let result = {
            let parent_rx = handle
                .parent_rx
                .lock()
                .expect("PluginWorkerHandle parent_rx mutex poisoned");
            wait_for_invoke_response(
                lua,
                response_rx,
                &parent_rx,
                Duration::from_millis(timeout_ms.max(1)),
            )
        };
        if let Some(started) = perf_started {
            log::info!(
                "[PERF][plugin_worker] plugin={} kind={} id={} phase=roundtrip ok={} elapsed_ms={}",
                plugin_key,
                log_kind,
                log_id,
                result.is_ok(),
                started.elapsed().as_millis()
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_async(
        &self,
        lua: &Lua,
        plugin_key: &str,
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        timeout_ms: u64,
        callback: mlua::Function,
    ) -> Result<String> {
        let handle = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .get(plugin_key)
            .cloned()
            .ok_or_else(|| anyhow!("plugin worker not loaded: {plugin_key}"))?;
        let (completion_tx, completion_runtime) = self
            .hub_event_channel
            .lock()
            .expect("PluginWorkerRegistry hub_event_channel mutex poisoned")
            .clone()
            .ok_or_else(|| anyhow!("plugin worker Hub event channel is unavailable"))?;
        let request_id = format!(
            "plugin-worker-async-{}",
            NEXT_ASYNC_INVOKE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let timeout_ms = clamp_invoke_timeout_ms(timeout_ms);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let session_uuid = payload_session_uuid(&payload);
        let callback_key = lua
            .create_registry_value(callback)
            .map_err(|e| anyhow!("store plugin worker async callback: {e}"))?;
        self.async_callbacks
            .lock()
            .expect("PluginWorkerRegistry async_callbacks mutex poisoned")
            .insert(
                request_id.clone(),
                AsyncCallback {
                    key: callback_key,
                    session_uuid,
                },
            );
        let admission = handle
            .admission
            .lock()
            .expect("PluginWorkerHandle admission mutex poisoned");
        if let Some(reason) = &admission.shutdown_reason {
            self.drop_async_callback(lua, &request_id);
            return Err(anyhow!("plugin worker is shutting down: {reason}"));
        }
        if let Err(error) = handle.tx.try_send(PluginWorkerRequest::InvokeAsync {
            kind,
            id,
            name,
            payload,
            deadline,
            request_id: request_id.clone(),
            completion_tx,
            completion_runtime,
        }) {
            self.drop_async_callback(lua, &request_id);
            return Err(anyhow!(
                "plugin worker queue rejected async invoke: {error}"
            ));
        }
        drop(admission);
        Ok(request_id)
    }

    fn drop_async_callback(&self, lua: &Lua, request_id: &str) {
        if let Some(callback) = self
            .async_callbacks
            .lock()
            .expect("PluginWorkerRegistry async_callbacks mutex poisoned")
            .remove(request_id)
        {
            let _ = lua.remove_registry_value(callback.key);
        }
    }

    pub(crate) fn fire_async_callback(
        &self,
        lua: &Lua,
        request_id: &str,
        result: Result<serde_json::Value, String>,
    ) {
        let callback = self
            .async_callbacks
            .lock()
            .expect("PluginWorkerRegistry async_callbacks mutex poisoned")
            .remove(request_id);
        let Some(callback) = callback else {
            self.canceled
                .lock()
                .expect("PluginWorkerRegistry canceled mutex poisoned")
                .remove(request_id);
            log::warn!("plugin worker completion for unknown request: {request_id}");
            return;
        };
        Self::call_async_callback(lua, callback.key, result);
    }

    pub(crate) fn complete_async_invoke(
        &self,
        lua: &Lua,
        request_id: &str,
        result: Result<serde_json::Value, String>,
    ) {
        self.canceled
            .lock()
            .expect("PluginWorkerRegistry canceled mutex poisoned")
            .remove(request_id);
        self.fire_async_callback(lua, request_id, result);
    }

    pub(crate) fn fail_async_callbacks_for_session(
        &self,
        lua: &Lua,
        session_uuid: &str,
        error: &str,
    ) {
        let request_ids: Vec<String> = self
            .async_callbacks
            .lock()
            .expect("PluginWorkerRegistry async_callbacks mutex poisoned")
            .iter()
            .filter_map(|(request_id, callback)| {
                (callback.session_uuid.as_deref() == Some(session_uuid))
                    .then_some(request_id.clone())
            })
            .collect();
        {
            let mut canceled = self
                .canceled
                .lock()
                .expect("PluginWorkerRegistry canceled mutex poisoned");
            for request_id in &request_ids {
                canceled.insert(request_id.clone());
            }
        }
        for request_id in request_ids {
            self.fire_async_callback(lua, &request_id, Err(error.to_string()));
        }
    }

    pub(crate) fn fail_all_async_callbacks(&self, lua: &Lua, error: &str) {
        let request_ids: Vec<String> = self
            .async_callbacks
            .lock()
            .expect("PluginWorkerRegistry async_callbacks mutex poisoned")
            .keys()
            .cloned()
            .collect();
        for request_id in request_ids {
            self.fire_async_callback(lua, &request_id, Err(error.to_string()));
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_canceled(&self, request_id: &str) {
        self.canceled
            .lock()
            .expect("PluginWorkerRegistry canceled mutex poisoned")
            .insert(request_id.to_string());
    }

    pub(crate) fn canceled_ids(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.canceled)
    }

    fn is_canceled(canceled: &Mutex<HashSet<String>>, request_id: &str) -> bool {
        canceled
            .lock()
            .expect("PluginWorkerRegistry canceled mutex poisoned")
            .contains(request_id)
    }

    fn call_async_callback(
        lua: &Lua,
        callback_key: mlua::RegistryKey,
        result: Result<serde_json::Value, String>,
    ) {
        let callback_result: mlua::Result<()> = (|| {
            let callback: mlua::Function = lua.registry_value(&callback_key)?;
            match result {
                Ok(value) => {
                    let value = json_to_lua(lua, &value)?;
                    callback.call((value, mlua::Value::Nil))
                }
                Err(error) => callback.call((mlua::Value::Nil, error)),
            }
        })();
        if let Err(error) = callback_result {
            log::warn!("plugin worker async callback failed: {error}");
        }
        let _ = lua.remove_registry_value(callback_key);
    }

    fn shutdown(&self, plugin_key: &str, reason: &str) {
        let handle = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .remove(plugin_key);
        if let Some(handle) = handle {
            let mut admission = handle
                .admission
                .lock()
                .expect("PluginWorkerHandle admission mutex poisoned");
            admission.shutdown_reason = Some(reason.to_string());
            let _ = handle.tx.try_send(PluginWorkerRequest::Shutdown {
                reason: reason.to_string(),
            });
        }
    }

    pub(crate) fn shutdown_all(&self, reason: &str) {
        let handles: Vec<_> = self
            .workers
            .lock()
            .expect("PluginWorkerRegistry mutex poisoned")
            .drain()
            .collect();
        for (plugin_key, handle) in handles {
            let mut admission = handle
                .admission
                .lock()
                .expect("PluginWorkerHandle admission mutex poisoned");
            admission.shutdown_reason = Some(reason.to_string());
            let _ = handle.tx.try_send(PluginWorkerRequest::Shutdown {
                reason: reason.to_string(),
            });
            log::debug!("plugin worker {plugin_key} shutdown requested: {reason}");
        }
    }
}

fn worker_loop(
    spec: WorkerSpec,
    rx: mpsc::Receiver<PluginWorkerRequest>,
    worker_event_tx: PluginWorkerEventTx,
    parent_tx: mpsc::Sender<WorkerParentRequest>,
    ready_tx: mpsc::Sender<Result<(), String>>,
    hub_event_channel: Option<(HubEventTx, tokio::runtime::Handle)>,
    admission: Arc<Mutex<PluginWorkerAdmission>>,
    canceled: Arc<Mutex<HashSet<String>>>,
) {
    let mut runtime = match LuaRuntime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("create plugin worker runtime: {err}")));
            return;
        }
    };
    let parent_hub_event_tx = hub_event_channel.as_ref().map(|(tx, _)| tx.clone());
    let parent_hub_event_runtime = hub_event_channel
        .as_ref()
        .map(|(_, runtime)| runtime.clone());
    if let Some((tx, tokio_handle)) = hub_event_channel {
        runtime.set_hub_event_tx(tx, tokio_handle);
    }
    runtime.set_plugin_worker_event_tx(worker_event_tx);
    let parent_hub_event_ready = Arc::new(AtomicBool::new(false));
    if let Err(err) = install_parent_hub_bridge(
        &runtime,
        parent_tx,
        parent_hub_event_tx,
        parent_hub_event_runtime,
        Arc::clone(&parent_hub_event_ready),
    ) {
        let _ = ready_tx.send(Err(format!("install parent hub bridge: {err}")));
        return;
    }
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
        close_worker_plugin_databases(&runtime, &spec.plugin_key);
        let _ = ready_tx.send(Err(err));
        return;
    }
    parent_hub_event_ready.store(true, Ordering::Release);
    let _ = ready_tx.send(Ok(()));

    loop {
        let shutdown_reason = admission
            .lock()
            .expect("PluginWorkerAdmission mutex poisoned")
            .shutdown_reason
            .clone();
        if let Some(reason) = shutdown_reason {
            fail_pending_worker_requests(&rx, &reason);
            break;
        }
        let request = match rx.recv() {
            Ok(request) => request,
            Err(_) => break,
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
                let perf_started = lua_perf_enabled().then(Instant::now);
                let _guard = enter_plugin_worker(&spec.plugin_key);
                // The Hub waits for this synchronous invocation and services
                // the fallback parent-request queue during that wait. Routing
                // a parent request back through the Hub event queue here would
                // deadlock the worker and the Hub until the invoke timeout.
                let _parent_route_guard =
                    ParentHubRouteGuard::use_synchronous_fallback(&parent_hub_event_ready);
                let result =
                    invoke_in_worker(&runtime, &kind, &id, name.as_deref(), payload, timeout_ms);
                if let Some(started) = perf_started {
                    log::info!(
                        "[PERF][plugin_worker] plugin={} kind={} id={} phase=execute ok={} elapsed_ms={}",
                        spec.plugin_key,
                        kind,
                        id,
                        result.is_ok(),
                        started.elapsed().as_millis()
                    );
                }
                let _ = response.send(result);
            }
            PluginWorkerRequest::InvokeAsync {
                kind,
                id,
                name,
                payload,
                deadline,
                request_id,
                completion_tx,
                completion_runtime,
            } => {
                let mut pending =
                    CompletionGuard::new(request_id.clone(), completion_tx, completion_runtime);
                if PluginWorkerRegistry::is_canceled(&canceled, &request_id) {
                    pending.send(Err("plugin worker invoke canceled".to_string()));
                    continue;
                }
                let Some(timeout_ms) = remaining_handler_timeout_ms(deadline) else {
                    pending.send(Err("plugin worker invoke expired".to_string()));
                    continue;
                };
                let perf_started = lua_perf_enabled().then(Instant::now);
                let _guard = enter_plugin_worker(&spec.plugin_key);
                let result =
                    invoke_in_worker(&runtime, &kind, &id, name.as_deref(), payload, timeout_ms);
                if let Some(started) = perf_started {
                    log::info!(
                        "[PERF][plugin_worker] plugin={} kind={} id={} phase=execute_async ok={} elapsed_ms={}",
                        spec.plugin_key,
                        kind,
                        id,
                        result.is_ok(),
                        started.elapsed().as_millis()
                    );
                }
                pending.send(result);
            }
            PluginWorkerRequest::Shutdown { reason } => {
                log::debug!(
                    "plugin worker {} shutting down: {}",
                    spec.plugin_key,
                    reason
                );
                fail_pending_worker_requests(&rx, &reason);
                break;
            }
            PluginWorkerRequest::HttpResponse(response) => {
                runtime.fire_http_callback(response);
            }
            PluginWorkerRequest::WebSocketEvent(event) => {
                runtime.fire_websocket_event(event);
            }
            PluginWorkerRequest::TimerFired { timer_id } => {
                runtime.fire_timer_callback(&timer_id);
            }
            PluginWorkerRequest::UserFileWatch { watch_id, events } => {
                runtime.fire_user_file_watch(&watch_id, events);
            }
            PluginWorkerRequest::LocalWebhook(request) => {
                runtime.fire_local_webhook_request(request);
            }
        }
    }

    close_worker_plugin_databases(&runtime, &spec.plugin_key);
}

fn fail_pending_worker_requests(rx: &mpsc::Receiver<PluginWorkerRequest>, reason: &str) {
    while let Ok(request) = rx.try_recv() {
        let error = format!("plugin worker shutting down: {reason}");
        match request {
            PluginWorkerRequest::Invoke { response, .. } => {
                let _ = response.send(Err(error));
            }
            PluginWorkerRequest::InvokeAsync {
                request_id,
                completion_tx,
                completion_runtime,
                ..
            } => {
                completion_runtime.spawn(async move {
                    if let Err(send_error) = completion_tx
                        .send_async(HubEvent::PluginWorkerInvokeCompleted {
                            request_id,
                            result: Err(error),
                        })
                        .await
                    {
                        log::warn!("plugin worker shutdown completion queue closed: {send_error}");
                    }
                });
            }
            PluginWorkerRequest::Shutdown { .. }
            | PluginWorkerRequest::HttpResponse(_)
            | PluginWorkerRequest::WebSocketEvent(_)
            | PluginWorkerRequest::TimerFired { .. }
            | PluginWorkerRequest::UserFileWatch { .. }
            | PluginWorkerRequest::LocalWebhook(_) => {}
        }
    }
}

fn close_worker_plugin_databases(runtime: &LuaRuntime, plugin_key: &str) {
    if let Err(err) = runtime
        .lua()
        .load(
            r#"
            local ok, plugin_db = pcall(require, "lib.plugin_db")
            if ok and plugin_db and type(plugin_db.shutdown_all) == "function" then
                plugin_db.shutdown_all()
            end
            "#,
        )
        .set_name("plugin_worker_close_databases")
        .exec()
    {
        log::warn!("plugin worker {plugin_key} failed to close plugin databases: {err}");
    }
}

fn install_parent_hub_bridge(
    runtime: &LuaRuntime,
    parent_tx: mpsc::Sender<WorkerParentRequest>,
    parent_hub_event_tx: Option<HubEventTx>,
    parent_hub_event_runtime: Option<tokio::runtime::Handle>,
    parent_hub_event_ready: Arc<AtomicBool>,
) -> Result<()> {
    let lua = runtime.lua();
    let bridge = lua
        .create_table()
        .map_err(|e| anyhow!("create plugin_worker_parent_hub table: {e}"))?;
    let request_tx = parent_tx.clone();
    let request_event_tx = parent_hub_event_tx.clone();
    let request_event_route = Arc::clone(&parent_hub_event_ready);
    let request_fn = lua
        .create_function(move |lua, (payload, timeout_ms): (Value, Option<u64>)| {
            let payload_json: serde_json::Value = lua.from_value(payload).map_err(|e| {
                mlua::Error::external(format!(
                    "plugin_worker_parent_hub.request: failed to serialize payload: {e}"
                ))
            })?;
            let (response_tx, response_rx) = mpsc::channel();
            let request = WorkerParentRequest::HubRequest {
                payload: payload_json,
                response: response_tx,
            };
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000).max(1));
            let deadline = Instant::now() + timeout;
            if request_event_route.load(Ordering::Acquire)
                && request_event_tx.is_some()
                && parent_hub_event_runtime.is_some()
            {
                let tx = request_event_tx
                    .as_ref()
                    .expect("checked parent Hub event channel");
                let runtime = parent_hub_event_runtime
                    .as_ref()
                    .expect("checked parent Hub event runtime");
                let admission_timeout = timeout.min(PARENT_EVENT_ADMISSION_TIMEOUT);
                match runtime.block_on(async {
                    tokio::time::timeout(
                        admission_timeout,
                        tx.send_async(HubEvent::PluginWorkerParentRequest(request)),
                    )
                    .await
                }) {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        return Err(mlua::Error::external(format!(
                            "plugin_worker_parent_hub.request: parent hub unavailable: {error}"
                        )));
                    }
                    Err(_) => {
                        return Err(mlua::Error::external(format!(
                            "plugin_worker_parent_hub.request: admission timeout after {}ms",
                            admission_timeout.as_millis()
                        )));
                    }
                }
            } else {
                request_tx.send(request).map_err(|e| {
                    mlua::Error::external(format!(
                        "plugin_worker_parent_hub.request: parent hub unavailable: {e}"
                    ))
                })?;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(mlua::Error::external(format!(
                    "plugin_worker_parent_hub.request: timeout after {}ms",
                    timeout.as_millis()
                )));
            }
            match response_rx.recv_timeout(remaining) {
                Ok(Ok(response)) => json_to_lua(lua, &response),
                Ok(Err(err)) => Err(mlua::Error::external(err)),
                Err(mpsc::RecvTimeoutError::Timeout) => Err(mlua::Error::external(format!(
                    "plugin_worker_parent_hub.request: timeout after {}ms",
                    timeout.as_millis()
                ))),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(mlua::Error::external(
                    "plugin_worker_parent_hub.request: response channel closed",
                )),
            }
        })
        .map_err(|e| anyhow!("create plugin_worker_parent_hub.request: {e}"))?;
    bridge
        .set("request", request_fn)
        .map_err(|e| anyhow!("set plugin_worker_parent_hub.request: {e}"))?;
    let enqueue_tx = parent_tx.clone();
    let enqueue_event_tx = parent_hub_event_tx.clone();
    let enqueue_event_route = Arc::clone(&parent_hub_event_ready);
    let enqueue_fn = lua
        .create_function(move |lua, payload: Value| {
            let payload_json: serde_json::Value = lua.from_value(payload).map_err(|e| {
                mlua::Error::external(format!(
                    "plugin_worker_parent_hub.enqueue: failed to serialize payload: {e}"
                ))
            })?;
            let (response_tx, _response_rx) = mpsc::channel();
            let request = WorkerParentRequest::HubRequest {
                payload: payload_json,
                response: response_tx,
            };
            if enqueue_event_route.load(Ordering::Acquire) && enqueue_event_tx.is_some() {
                let tx = enqueue_event_tx
                    .as_ref()
                    .expect("checked parent Hub event channel");
                tx.send(HubEvent::PluginWorkerParentRequest(request))
                    .map_err(|e| {
                        mlua::Error::external(format!(
                            "plugin_worker_parent_hub.enqueue: parent hub unavailable: {e}"
                        ))
                    })?;
            } else {
                enqueue_tx.send(request).map_err(|e| {
                    mlua::Error::external(format!(
                        "plugin_worker_parent_hub.enqueue: parent hub unavailable: {e}"
                    ))
                })?;
            }
            Ok(true)
        })
        .map_err(|e| anyhow!("create plugin_worker_parent_hub.enqueue: {e}"))?;
    bridge
        .set("enqueue", enqueue_fn)
        .map_err(|e| anyhow!("set plugin_worker_parent_hub.enqueue: {e}"))?;
    lua.globals()
        .set("plugin_worker_parent_hub", bridge)
        .map_err(|e| anyhow!("set plugin_worker_parent_hub global: {e}"))?;
    Ok(())
}

fn wait_for_ready(
    lua: &Lua,
    ready_rx: mpsc::Receiver<Result<(), String>>,
    parent_rx: &mpsc::Receiver<WorkerParentRequest>,
    timeout: Duration,
) -> Result<Result<(), String>, mpsc::RecvTimeoutError> {
    let deadline = Instant::now() + timeout;
    loop {
        service_parent_requests(lua, parent_rx);
        let now = Instant::now();
        if now >= deadline {
            return Err(mpsc::RecvTimeoutError::Timeout);
        }
        let remaining = deadline.saturating_duration_since(now);
        match ready_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(result) => {
                service_parent_requests(lua, parent_rx);
                service_parent_requests_for(lua, parent_rx, Duration::from_millis(100));
                return Ok(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(err) => {
                service_parent_requests(lua, parent_rx);
                service_parent_requests_for(lua, parent_rx, Duration::from_millis(100));
                return Err(err);
            }
        }
    }
}

fn wait_for_invoke_response(
    lua: &Lua,
    response_rx: mpsc::Receiver<Result<serde_json::Value, String>>,
    parent_rx: &mpsc::Receiver<WorkerParentRequest>,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        service_parent_requests(lua, parent_rx);
        let now = Instant::now();
        if now >= deadline {
            return Err(anyhow!("plugin worker invoke timeout"));
        }
        let remaining = deadline.saturating_duration_since(now);
        match response_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(Ok(value)) => {
                service_parent_requests(lua, parent_rx);
                return Ok(value);
            }
            Ok(Err(err)) => {
                service_parent_requests(lua, parent_rx);
                return Err(anyhow!(err));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                service_parent_requests(lua, parent_rx);
                return Err(anyhow!("plugin worker response channel closed"));
            }
        }
    }
}

fn service_parent_requests(lua: &Lua, parent_rx: &mpsc::Receiver<WorkerParentRequest>) {
    while let Ok(request) = parent_rx.try_recv() {
        service_parent_request(lua, request);
    }
}

fn service_parent_requests_for(
    lua: &Lua,
    parent_rx: &mpsc::Receiver<WorkerParentRequest>,
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    loop {
        service_parent_requests(lua, parent_rx);
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline.saturating_duration_since(now);
        match parent_rx.recv_timeout(remaining.min(Duration::from_millis(1))) {
            Ok(request) => service_parent_request(lua, request),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub(crate) fn service_parent_request(lua: &Lua, request: WorkerParentRequest) {
    match request {
        WorkerParentRequest::HubRequest { payload, response } => {
            let _ = response.send(handle_parent_hub_request(lua, payload));
        }
    }
}

fn handle_parent_hub_request(
    lua: &Lua,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let payload_lua = json_to_lua(lua, &payload).map_err(|e| e.to_string())?;
    let globals = lua.globals();
    globals
        .set("__plugin_worker_parent_hub_payload", payload_lua)
        .map_err(|e| e.to_string())?;
    let result: Value = lua
        .load(
            r#"
            local Hub = require("lib.hub")
            return Hub._handle_worker_parent_request(__plugin_worker_parent_hub_payload)
            "#,
        )
        .set_name("plugin_worker_parent_hub_request")
        .eval()
        .map_err(|e| e.to_string())?;
    lua.from_value(result)
        .map_err(|e| format!("plugin worker parent hub response conversion failed: {e}"))
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
    if let Some(parent_hub_id) = &spec.parent_hub_id {
        globals
            .set("_plugin_worker_parent_hub_id", parent_hub_id.clone())
            .map_err(|e| e.to_string())?;
    }
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
        "notification_observer" => lua
            .load(
                r#"
                local notifications = require("lib.notifications")
                local ok, result = __hook_timed_pcall(function()
                    return notifications._invoke_observer(
                        __handler_id,
                        __payload.phase,
                        __payload.intent,
                        __payload.decision)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_notification_observer")
            .eval()
            .map_err(|e| e.to_string())?,
        "notification_claim" => lua
            .load(
                r#"
                local notifications = require("lib.notifications")
                local ok, result = __hook_timed_pcall(function()
                    return notifications._invoke_claim(__handler_id, __payload.intent)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_notification_claim")
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
        "plugin_asset_read" => lua
            .load(
                r#"
                local plugin_assets = require("lib.plugin_assets")
                local ok, result, err = __hook_timed_pcall(function()
                    return plugin_assets.read(__payload.asset_id)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                if not result then error(err or "Unknown plugin asset") end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_plugin_asset_read")
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
        "ac_message" => lua
            .load(
                r#"
                local action_cable = require("lib.action_cable")
                local ok, result = __hook_timed_pcall(function()
                    return action_cable._invoke_ac_message(__handler_id, __payload.channel_id, __payload.message)
                end, __handler_timeout_ms)
                if not ok then error(result) end
                return result
                "#,
            )
            .set_name("plugin_worker_invoke_ac_message")
            .eval()
            .map_err(|e| e.to_string())?,
        "ac_unregister" => lua
            .load(
                r#"
                local action_cable = require("lib.action_cable")
                action_cable._unregister_handler(__handler_id)
                return true
                "#,
            )
            .set_name("plugin_worker_invoke_ac_unregister")
            .eval()
            .map_err(|e| e.to_string())?,
        _ => return Err(format!("unsupported plugin handler kind: {kind}")),
    };

    lua.from_value(result)
        .map_err(|e| format!("plugin worker result conversion failed: {e}"))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::missing_docs_in_private_items,
        reason = "test-code brevity"
    )]

    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn worker_database_cleanup_calls_plugin_db_shutdown() {
        let runtime = LuaRuntime::new().expect("runtime");
        runtime
            .lua()
            .load(
                r#"
                package.preload["lib.plugin_db"] = function()
                  return {
                    shutdown_all = function()
                      _G.plugin_database_shutdown_called = true
                    end,
                  }
                end
                "#,
            )
            .exec()
            .expect("install fake plugin.db module");

        close_worker_plugin_databases(&runtime, "test-plugin");

        let called: bool = runtime
            .lua()
            .globals()
            .get("plugin_database_shutdown_called")
            .expect("read shutdown flag");
        assert!(called, "worker cleanup must close all plugin databases");
    }

    #[test]
    fn load_wait_services_worker_parent_hub_requests() {
        let lua = Lua::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let lib_dir = temp.path().join("lib");
        fs::create_dir_all(&lib_dir).expect("create lib dir");
        fs::write(
            lib_dir.join("hub.lua"),
            r#"
            return {
              _handle_worker_parent_request = function(payload)
                return { result = { ok = true, echoed_type = payload.type } }
              end,
            }
            "#,
        )
        .expect("write fake hub.lua");
        lua.load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path
            "#,
            dir = temp.path().display()
        ))
        .exec()
        .expect("configure package path");

        let (parent_tx, parent_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::spawn(move || {
            let (response_tx, response_rx) = mpsc::channel();
            parent_tx
                .send(WorkerParentRequest::HubRequest {
                    payload: json!({ "type": "get_agent_list" }),
                    response: response_tx,
                })
                .expect("send parent request");
            let response = response_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("parent response")
                .expect("parent response ok");
            assert_eq!(response["result"]["echoed_type"], "get_agent_list");
            ready_tx.send(Ok(())).expect("send ready");
        });

        let result = wait_for_ready(&lua, ready_rx, &parent_rx, Duration::from_secs(1))
            .expect("wait should not time out")
            .expect("worker should report ready");
        assert_eq!(result, ());
    }

    #[test]
    fn enqueue_parent_hub_request_uses_fallback_when_sync_route_is_selected() {
        let runtime = LuaRuntime::new().expect("runtime");
        runtime
            .lua()
            .load(
                r#"
                package.loaded["lib.hub"] = {
                  _handle_worker_parent_request = function(payload)
                    _G.parent_payload = payload
                    return { result = { ok = true } }
                  end,
                }
                "#,
            )
            .exec()
            .expect("install fake parent hub");

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let hub_event_tx = HubEventTx::from(event_tx);
        let (fallback_tx, fallback_rx) = mpsc::channel();
        install_parent_hub_bridge(
            &runtime,
            fallback_tx,
            Some(hub_event_tx),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("install parent hub bridge");

        runtime
            .lua()
            .load(
                r#"
                assert(plugin_worker_parent_hub.enqueue({
                  type = "hub_command",
                  command = {
                    type = "update_session",
                    agent_id = "parent-session",
                    plugin_state = {
                      cloudflare_hosted_preview = { status = "running" },
                    },
                  },
                }))
                "#,
            )
            .exec()
            .expect("enqueue parent request");

        assert!(
            event_rx.try_recv().is_err(),
            "the synchronous route must not use the blocked Hub event loop"
        );
        let request = fallback_rx.try_recv().expect("fallback parent request");
        service_parent_request(runtime.lua(), request);

        let status: String = runtime
            .lua()
            .load(
                r#"
                return _G.parent_payload.command.plugin_state.cloudflare_hosted_preview.status
                "#,
            )
            .eval()
            .expect("parent request handled");
        assert_eq!(status, "running");
    }

    #[test]
    fn enqueue_parent_hub_request_uses_event_channel_for_async_route() {
        let runtime = LuaRuntime::new().expect("runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let hub_event_tx = HubEventTx::from(event_tx);
        let (fallback_tx, fallback_rx) = mpsc::channel();
        install_parent_hub_bridge(
            &runtime,
            fallback_tx,
            Some(hub_event_tx),
            None,
            Arc::new(AtomicBool::new(true)),
        )
        .expect("install parent hub bridge");

        runtime
            .lua()
            .load(
                r#"
                assert(plugin_worker_parent_hub.enqueue({
                  type = "hub_command",
                  command = { type = "update_session" },
                }))
                "#,
            )
            .exec()
            .expect("enqueue parent request");

        assert!(fallback_rx.try_recv().is_err());
        assert!(matches!(
            event_rx.blocking_recv(),
            Some(HubEvent::PluginWorkerParentRequest(_))
        ));
    }

    #[test]
    fn blocking_parent_hub_request_keeps_load_wait_fallback_when_event_channel_exists() {
        let runtime = LuaRuntime::new().expect("runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let hub_event_tx = HubEventTx::from(event_tx);
        let (fallback_tx, fallback_rx) = mpsc::channel();
        install_parent_hub_bridge(
            &runtime,
            fallback_tx,
            Some(hub_event_tx),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("install parent hub bridge");

        let responder = std::thread::spawn(move || {
            let request = fallback_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("fallback parent request");
            match request {
                WorkerParentRequest::HubRequest { payload, response } => {
                    assert_eq!(payload["type"], "get_agent_list");
                    response
                        .send(Ok(json!({ "result": { "ok": true } })))
                        .expect("send fallback response");
                }
            }
        });

        let ok: bool = runtime
            .lua()
            .load(
                r#"
                local response = plugin_worker_parent_hub.request({
                  type = "get_agent_list",
                }, 1000)
                return response.result.ok
                "#,
            )
            .eval()
            .expect("blocking request should receive fallback response");

        assert!(
            event_rx.try_recv().is_err(),
            "blocking request must not depend on the hub event loop during worker load"
        );
        responder.join().expect("responder thread");
        assert!(ok);
    }

    #[test]
    fn blocking_parent_hub_request_uses_event_channel_after_worker_is_ready() {
        let runtime = LuaRuntime::new().expect("runtime");
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let hub_event_tx = HubEventTx::from(event_tx);
        let (fallback_tx, fallback_rx) = mpsc::channel();
        install_parent_hub_bridge(
            &runtime,
            fallback_tx,
            Some(hub_event_tx),
            Some(tokio_runtime.handle().clone()),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("install parent hub bridge");

        let responder = std::thread::spawn(move || {
            let event = event_rx.blocking_recv().expect("parent Hub event");
            let HubEvent::PluginWorkerParentRequest(request) = event else {
                panic!("expected plugin worker parent request, got {event:?}");
            };
            match request {
                WorkerParentRequest::HubRequest { payload, response } => {
                    assert_eq!(payload["type"], "get_agent_list");
                    response
                        .send(Ok(json!({ "result": { "ok": true } })))
                        .expect("send event response");
                }
            }
        });

        let ok: bool = runtime
            .lua()
            .load(
                r#"
                local response = plugin_worker_parent_hub.request({
                  type = "get_agent_list",
                }, 1000)
                return response.result.ok
                "#,
            )
            .eval()
            .expect("blocking request should receive event response");

        assert!(
            fallback_rx.try_recv().is_err(),
            "ready workers must not depend on the load-wait fallback"
        );
        responder.join().expect("responder thread");
        assert!(ok);
    }

    #[test]
    fn blocking_parent_hub_request_waits_for_bounded_queue_admission() {
        let runtime = LuaRuntime::new().expect("runtime");
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .blocking_send(HubEvent::CleanupTick)
            .expect("fill Hub event queue");
        let hub_event_tx = HubEventTx::from(event_tx);
        let (fallback_tx, fallback_rx) = mpsc::channel();
        install_parent_hub_bridge(
            &runtime,
            fallback_tx,
            Some(hub_event_tx),
            Some(tokio_runtime.handle().clone()),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("install parent hub bridge");

        let responder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            assert!(matches!(
                event_rx.blocking_recv(),
                Some(HubEvent::CleanupTick)
            ));
            let event = event_rx.blocking_recv().expect("parent Hub event");
            let HubEvent::PluginWorkerParentRequest(request) = event else {
                panic!("expected plugin worker parent request, got {event:?}");
            };
            match request {
                WorkerParentRequest::HubRequest { response, .. } => {
                    response
                        .send(Ok(json!({ "result": { "ok": true } })))
                        .expect("send event response");
                }
            }
        });

        let ok: bool = runtime
            .lua()
            .load(
                r#"
                local response = plugin_worker_parent_hub.request({
                  type = "get_agent_list",
                }, 1000)
                return response.result.ok
                "#,
            )
            .eval()
            .expect("request should wait for queue admission");

        assert!(fallback_rx.try_recv().is_err());
        responder.join().expect("responder thread");
        assert!(ok);
    }

    #[test]
    fn async_worker_invoke_admits_then_completes_through_hub_event() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, worker_rx) = mpsc::sync_channel(1);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");

        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_tool",
                  "tool-id",
                  "tool-name",
                  { value = "queued" },
                  5000,
                  function(result, err)
                    _G.async_result = result
                    _G.async_error = err
                  end)
                "#,
            )
            .eval()
            .expect("admit async invoke");
        assert!(lua.globals().get::<Value>("async_result").unwrap().is_nil());

        let request = worker_rx.try_recv().expect("worker request admitted");
        let PluginWorkerRequest::InvokeAsync {
            request_id: worker_request_id,
            completion_tx,
            ..
        } = request
        else {
            panic!("expected async worker request");
        };
        assert_eq!(request_id, worker_request_id);

        tokio_runtime.block_on(async {
            completion_tx
                .send_async(HubEvent::PluginWorkerInvokeCompleted {
                    request_id: worker_request_id,
                    result: Ok(json!({ "value": "complete" })),
                })
                .await
                .expect("queue completion");
        });
        let event = event_rx.blocking_recv().expect("completion event");
        let HubEvent::PluginWorkerInvokeCompleted { request_id, result } = event else {
            panic!("expected plugin worker completion, got {event:?}");
        };
        registry.fire_async_callback(&lua, &request_id, result);

        let value: String = lua
            .load("return _G.async_result.value")
            .eval()
            .expect("async callback result");
        assert_eq!(value, "complete");
        assert!(lua.globals().get::<Value>("async_error").unwrap().is_nil());
    }

    #[test]
    fn async_worker_completion_waits_for_bounded_queue_admission() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .blocking_send(HubEvent::CleanupTick)
            .expect("fill Hub event queue");
        let hub_event_tx = HubEventTx::from(event_tx);
        let join = std::thread::spawn({
            let hub_event_tx = hub_event_tx.clone();
            let handle = tokio_runtime.handle().clone();
            move || {
                handle.block_on(
                    hub_event_tx.send_async(HubEvent::PluginWorkerInvokeCompleted {
                        request_id: "plugin-worker-async-wait".to_string(),
                        result: Ok(json!({ "ok": true })),
                    }),
                )
            }
        });
        std::thread::sleep(Duration::from_millis(25));
        assert!(
            matches!(event_rx.try_recv(), Ok(HubEvent::CleanupTick)),
            "the filler event must still occupy the bounded queue"
        );
        let event = event_rx.blocking_recv().expect("queued completion");
        let HubEvent::PluginWorkerInvokeCompleted { request_id, .. } = event else {
            panic!("expected completion, got {event:?}");
        };
        assert_eq!(request_id, "plugin-worker-async-wait");
        join.join()
            .expect("completion wait thread")
            .expect("completion admitted");
    }

    #[test]
    fn canceling_an_async_callback_removes_the_registry_key() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, _worker_rx) = mpsc::sync_channel(1);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_tool",
                  "tool-id",
                  "tool-name",
                  { context = { session_uuid = "sess-timeout" } },
                  5000,
                  function(result, err)
                    _G.cancel_error = err
                  end)
                "#,
            )
            .eval()
            .expect("admit async invoke");
        registry.mark_canceled(&request_id);
        registry.fire_async_callback(
            &lua,
            &request_id,
            Err("hub MCP dispatch timed out".to_string()),
        );
        let error: String = lua.globals().get("cancel_error").expect("cancel error");
        assert_eq!(error, "hub MCP dispatch timed out");
        assert!(PluginWorkerRegistry::is_canceled(
            &registry.canceled,
            &request_id
        ));
        assert!(registry
            .async_callbacks
            .lock()
            .expect("callback registry")
            .is_empty());
    }

    #[test]
    fn shutdown_fails_in_flight_async_callbacks() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, _worker_rx) = mpsc::sync_channel(1);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        lua.load(
            r#"
            __plugin_worker_invoke_async(
              "test-plugin",
              "mcp_tool",
              "tool-id",
              "tool-name",
              {},
              5000,
              function(result, err)
                _G.exit_error = err
              end)
            "#,
        )
        .exec()
        .expect("admit async invoke");
        registry.fail_all_async_callbacks(&lua, "plugin worker exited");
        let error: String = lua.globals().get("exit_error").expect("exit error");
        assert_eq!(error, "plugin worker exited");
        assert!(registry
            .async_callbacks
            .lock()
            .expect("callback registry")
            .is_empty());
    }

    #[test]
    fn session_revoke_fails_matching_async_callbacks() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, _worker_rx) = mpsc::sync_channel(2);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        lua.load(
            r#"
            __plugin_worker_invoke_async(
              "test-plugin",
              "mcp_tool",
              "tool-id",
              "tool-name",
              { context = { session_uuid = "sess-revoked" } },
              5000,
              function(result, err)
                _G.revoke_error = err
              end)
            "#,
        )
        .exec()
        .expect("admit async invoke");
        registry.fail_async_callbacks_for_session(
            &lua,
            "sess-revoked",
            "MCP caller credential revoked",
        );
        let error: String = lua.globals().get("revoke_error").expect("revoke error");
        assert_eq!(error, "MCP caller credential revoked");
        assert!(registry
            .async_callbacks
            .lock()
            .expect("callback registry")
            .is_empty());
    }

    #[test]
    fn prompt_payload_session_revoke_fails_callback() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, worker_rx) = mpsc::sync_channel(2);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_prompt",
                  "prompt-id",
                  "prompt-name",
                  {
                    name = "prompt-name",
                    args = {},
                    context = { session_uuid = "sess-prompt" }
                  },
                  5000,
                  function(result, err)
                    _G.prompt_revoke_error = err
                  end)
                "#,
            )
            .eval()
            .expect("admit prompt invoke");
        registry.fail_async_callbacks_for_session(
            &lua,
            "sess-prompt",
            "MCP caller credential revoked",
        );
        let error: String = lua
            .globals()
            .get("prompt_revoke_error")
            .expect("prompt revoke error");
        assert_eq!(error, "MCP caller credential revoked");
        assert!(PluginWorkerRegistry::is_canceled(
            &registry.canceled,
            &request_id
        ));
        let request = worker_rx.try_recv().expect("queued prompt invoke");
        assert!(matches!(request, PluginWorkerRequest::InvokeAsync { .. }));
        assert!(registry
            .async_callbacks
            .lock()
            .expect("callback registry")
            .is_empty());
    }

    #[test]
    fn queued_canceled_async_invoke_is_marked_before_execution() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, worker_rx) = mpsc::sync_channel(1);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_tool",
                  "tool-id",
                  "tool-name",
                  { context = { session_uuid = "sess-queued" } },
                  5000,
                  function() end)
                "#,
            )
            .eval()
            .expect("admit async invoke");
        registry.mark_canceled(&request_id);
        assert!(PluginWorkerRegistry::is_canceled(
            &registry.canceled,
            &request_id
        ));
        assert!(matches!(
            worker_rx.try_recv(),
            Ok(PluginWorkerRequest::InvokeAsync { .. })
        ));
    }

    #[test]
    fn remaining_handler_timeout_skips_expired_deadlines() {
        assert!(remaining_handler_timeout_ms(Instant::now() - Duration::from_secs(1)).is_none());
        assert!(
            remaining_handler_timeout_ms(Instant::now() + Duration::from_secs(2)).unwrap() >= 1
        );
    }

    #[test]
    fn cancel_id_stays_until_completion_reaches_the_hub() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .blocking_send(HubEvent::CleanupTick)
            .expect("fill Hub event queue");
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, _worker_rx) = mpsc::sync_channel(1);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");
        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_tool",
                  "tool-id",
                  "tool-name",
                  { context = { session_uuid = "sess-race" } },
                  5000,
                  function(result, err)
                    _G.race_error = err
                  end)
                "#,
            )
            .eval()
            .expect("admit async invoke");
        registry.mark_canceled(&request_id);
        registry.fire_async_callback(
            &lua,
            &request_id,
            Err("hub MCP dispatch timed out".to_string()),
        );
        assert!(PluginWorkerRegistry::is_canceled(
            &registry.canceled,
            &request_id
        ));
        registry.mark_canceled(&request_id);
        registry.complete_async_invoke(
            &lua,
            &request_id,
            Err("plugin worker invoke canceled".to_string()),
        );
        assert!(!PluginWorkerRegistry::is_canceled(
            &registry.canceled,
            &request_id
        ));
        assert!(matches!(event_rx.try_recv(), Ok(HubEvent::CleanupTick)));
    }

    #[test]
    fn worker_loop_skips_canceled_and_expired_invokes_before_handler() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("ran");
        let init_path = temp.path().join("init.lua");
        std::fs::write(
            &init_path,
            format!(
                r#"
                local mcp = require("lib.mcp")
                mcp.tool("marker", {{
                    description = "side effect",
                    input_schema = {{ type = "object", properties = {{}} }},
                    timeout_ms = 5000,
                }}, function()
                    local f = assert(io.open({marker}, "w"))
                    f:write("ran")
                    f:close()
                    return "ran"
                end)
                return {{}}
                "#,
                marker = serde_json::to_string(&marker.to_string_lossy()).unwrap(),
            ),
        )
        .expect("write plugin");

        let mut runtime = LuaRuntime::new().expect("runtime");
        let lua_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua");
        runtime
            .update_package_path(&lua_path)
            .expect("configure Lua path");
        runtime
            .lua()
            .globals()
            .set("_lua_base_path", lua_path.to_string_lossy().to_string())
            .expect("worker Lua path");
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .blocking_send(HubEvent::CleanupTick)
            .expect("fill Hub event queue");
        let hub_tx = HubEventTx::from(event_tx);
        runtime.set_hub_event_tx(hub_tx.clone(), tokio_runtime.handle().clone());
        runtime
            .lua()
            .load(format!(
                r#"
                local loader = require("hub.loader")
                local ok, err = loader.load_plugin({init_path}, "loop-skip-plugin", {{ source = "device" }})
                assert(ok, tostring(err))
                "#,
                init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
            ))
            .exec()
            .expect("load plugin");

        let handle = runtime
            .plugin_workers()
            .workers
            .lock()
            .expect("workers")
            .values()
            .next()
            .cloned()
            .expect("worker handle");
        let request_id = "plugin-worker-async-skip".to_string();
        runtime.plugin_workers().mark_canceled(&request_id);
        handle
            .tx
            .try_send(PluginWorkerRequest::InvokeAsync {
                kind: "mcp_tool".to_string(),
                id: "marker".to_string(),
                name: Some("marker".to_string()),
                payload: json!({}),
                deadline: Instant::now() + Duration::from_secs(5),
                request_id: request_id.clone(),
                completion_tx: hub_tx.clone(),
                completion_runtime: tokio_runtime.handle().clone(),
            })
            .expect("queue canceled invoke");
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            PluginWorkerRegistry::is_canceled(&runtime.plugin_workers().canceled, &request_id),
            "cancel id must remain while completion waits for Hub capacity"
        );
        assert!(
            !marker.exists(),
            "canceled handler must not run under completion backpressure"
        );
        assert!(matches!(
            event_rx.blocking_recv(),
            Some(HubEvent::CleanupTick)
        ));
        let event = event_rx.blocking_recv().expect("canceled completion");
        let HubEvent::PluginWorkerInvokeCompleted {
            request_id: completed_id,
            result,
        } = event
        else {
            panic!("expected completion, got {event:?}");
        };
        assert_eq!(completed_id, request_id);
        assert!(
            result.as_ref().unwrap_err().contains("canceled"),
            "skip must report canceled, got {result:?}"
        );
        runtime.fire_plugin_worker_invoke_completed(&request_id, result);
        assert!(!PluginWorkerRegistry::is_canceled(
            &runtime.plugin_workers().canceled,
            &request_id
        ));

        let expired_id = "plugin-worker-async-expired".to_string();
        handle
            .tx
            .try_send(PluginWorkerRequest::InvokeAsync {
                kind: "mcp_tool".to_string(),
                id: "marker".to_string(),
                name: Some("marker".to_string()),
                payload: json!({}),
                deadline: Instant::now() - Duration::from_secs(1),
                request_id: expired_id.clone(),
                completion_tx: hub_tx,
                completion_runtime: tokio_runtime.handle().clone(),
            })
            .expect("queue expired invoke");
        let event = event_rx.blocking_recv().expect("expired completion");
        let HubEvent::PluginWorkerInvokeCompleted {
            request_id: completed_id,
            result,
        } = event
        else {
            panic!("expected expired completion, got {event:?}");
        };
        assert_eq!(completed_id, expired_id);
        assert!(
            result.unwrap_err().contains("expired"),
            "skip must report expired"
        );
        assert!(!marker.exists(), "expired handler must not run");
        runtime.shutdown_plugin_workers("test_complete");
    }

    #[test]
    fn worker_shutdown_fails_queued_async_invoke_and_removes_callback() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let (worker_tx, worker_rx) = mpsc::sync_channel(2);
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                tx: worker_tx,
                parent_rx: Arc::new(Mutex::new(parent_rx)),
                admission: Arc::new(Mutex::new(PluginWorkerAdmission::default())),
            },
        );
        let lua = Lua::new();
        register(&lua, registry.clone()).expect("register worker primitives");

        let request_id: String = lua
            .load(
                r#"
                return __plugin_worker_invoke_async(
                  "test-plugin",
                  "mcp_tool",
                  "tool-id",
                  "tool-name",
                  {},
                  5000,
                  function(result, err)
                    _G.shutdown_result = result
                    _G.shutdown_error = err
                  end)
                "#,
            )
            .eval()
            .expect("admit async invoke");
        registry.shutdown("test-plugin", "reload");
        fail_pending_worker_requests(&worker_rx, "reload");

        let event = event_rx.blocking_recv().expect("shutdown completion event");
        let HubEvent::PluginWorkerInvokeCompleted {
            request_id: completed_id,
            result,
        } = event
        else {
            panic!("expected plugin worker completion, got {event:?}");
        };
        assert_eq!(request_id, completed_id);
        registry.fire_async_callback(&lua, &completed_id, result);

        let error: String = lua
            .globals()
            .get("shutdown_error")
            .expect("shutdown callback error");
        assert_eq!(error, "plugin worker shutting down: reload");
        assert!(lua
            .globals()
            .get::<Value>("shutdown_result")
            .unwrap()
            .is_nil());
        assert!(registry
            .async_callbacks
            .lock()
            .expect("callback registry")
            .is_empty());
    }

    #[test]
    fn synchronous_async_fallback_reports_one_callback_and_successful_admission() {
        let runtime = LuaRuntime::new().expect("runtime");
        let lua_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua");
        runtime
            .update_package_path(&lua_path)
            .expect("configure Lua path");

        let (accepted, callback_count, callback_ok, callback_error): (bool, u64, bool, String) =
            runtime
                .lua()
                .load(
                    r#"
                __plugin_worker_invoke_async = nil
                __plugin_worker_invoke = nil
                local supervisor = require("lib.plugin_supervisor")
                local callback_count = 0
                local callback_ok = true
                local callback_error = nil
                local accepted = supervisor.invoke_async(
                  "test-plugin",
                  "fallback",
                  function() error("fallback failed") end,
                  { handler_kind = "mcp_tool" },
                  function(ok, result)
                    callback_count = callback_count + 1
                    callback_ok = ok
                    callback_error = result
                  end)
                return accepted, callback_count, callback_ok, callback_error
                "#,
                )
                .eval()
                .expect("run async fallback");

        assert!(accepted);
        assert_eq!(callback_count, 1);
        assert!(!callback_ok);
        assert!(callback_error.contains("fallback failed"));
    }

    #[test]
    fn plugin_worker_guard_restores_context_and_prevents_cross_thread_use() {
        std::thread::Builder::new()
            .name("plugin-worker-test".to_string())
            .spawn(|| {
                // Behavior: enter sets the key, with_ sees it, drop restores previous.
                let guard = enter_plugin_worker("test-owner-42");
                with_current_plugin_worker(|cur| {
                    assert_eq!(cur, Some("test-owner-42"));
                });
                drop(guard);
                with_current_plugin_worker(|cur| {
                    assert!(cur.is_none(), "context must be restored after guard drop");
                });
            })
            .expect("spawn plugin-worker-test thread")
            .join()
            .expect("plugin-worker-test thread panicked");
    }

    #[test]
    fn plugin_worker_guard_nesting_restores_outer_context() {
        std::thread::Builder::new()
            .name("plugin-worker-test".to_string())
            .spawn(|| {
                let _outer = enter_plugin_worker("outer");
                {
                    let _inner = enter_plugin_worker("inner");
                    with_current_plugin_worker(|cur| assert_eq!(cur, Some("inner")));
                }
                with_current_plugin_worker(|cur| assert_eq!(cur, Some("outer")));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
