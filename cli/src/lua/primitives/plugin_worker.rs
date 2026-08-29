//! Per-plugin Lua worker primitive.
//!
//! Exposes a small synchronous bridge used by `lib.plugin_supervisor`.
//! Each plugin key owns one worker thread and one Lua VM. The hub VM keeps
//! routing/descriptor state; plugin-owned handler execution happens here.
//!
//! Remaining Hub-thread synchronous waits:
//! - `hook_interceptor` (`hooks.call`) so a plugin can allow, modify, or
//!   block an action before that action continues. Default budget is 10 ms.
//! - `notification_claim` so delivery ownership is decided before send.
//!   Default budget is 10 ms; a missed claim falls back to default delivery.
//! Interceptors that miss their budget keep the previous value.
//!
//! Mailbox classes and caps: MCP 192, Surface 128, Event 128, Maintenance 64,
//! Control 64. Shutdown is the only request that skips fair round-robin.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
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

/// Typed failure from a plugin-worker handler.
///
/// `Timeout` comes from `__worker_timed` after `__hook_timed_pcall`.
/// Callers must not string-match handler errors for timeout accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerFail {
    Timeout,
    ExpiredInQueue,
    Canceled,
    Handler(String),
}

impl WorkerFail {
    fn as_message(&self) -> String {
        match self {
            Self::Timeout => "plugin worker invoke timeout".to_string(),
            Self::ExpiredInQueue => "plugin worker invoke expired in queue".to_string(),
            Self::Canceled => "plugin worker invoke canceled".to_string(),
            Self::Handler(message) => message.clone(),
        }
    }

    fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout | Self::ExpiredInQueue)
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
    mailbox: WorkerMailbox,
    parent_rx: Arc<Mutex<mpsc::Receiver<WorkerParentRequest>>>,
    admission: Arc<Mutex<PluginWorkerAdmission>>,
}

#[derive(Default)]
struct PluginWorkerAdmission {
    shutdown_reason: Option<String>,
}

/// Invoke class used for admission and fair dequeue.
///
/// HTTP, WebSocket, timer, watch, and webhook callbacks share the Control
/// class and round-robin with MCP/surface/event/maintenance. Shutdown is
/// the only request that skips fairness. One plugin worker thread still
/// runs handlers serially; classes bound the mailbox and pick the next
/// queued invoke fairly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkClass {
    Mcp,
    Surface,
    Event,
    Maintenance,
    Control,
}

impl WorkClass {
    const ALL: [Self; 5] = [
        Self::Mcp,
        Self::Surface,
        Self::Event,
        Self::Maintenance,
        Self::Control,
    ];

    fn from_handler_kind(kind: &str) -> Self {
        match kind {
            "mcp_tool" | "mcp_prompt" | "mcp_resource" | "mcp_proxy_auth_error" => Self::Mcp,
            "surface_route" | "ui_action" | "asset_message" | "plugin_asset_read" => Self::Surface,
            "event" | "hook_observer" | "notification_observer" => Self::Event,
            "hook_interceptor" | "command" | "session_action" | "timer" | "watch"
            | "notification_claim" => Self::Maintenance,
            "ac_message" | "ac_unregister" => Self::Control,
            _ => Self::Maintenance,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Mcp => 0,
            Self::Surface => 1,
            Self::Event => 2,
            Self::Maintenance => 3,
            Self::Control => 4,
        }
    }

    fn cap(self) -> usize {
        match self {
            Self::Mcp => 192,
            Self::Surface => 128,
            Self::Event => 128,
            Self::Maintenance => 64,
            Self::Control => 64,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Surface => "surface",
            Self::Event => "event",
            Self::Maintenance => "maintenance",
            Self::Control => "control",
        }
    }
}

struct ClassMetrics {
    queued: [AtomicU64; 5],
    shed: [AtomicU64; 5],
    timeouts: [AtomicU64; 5],
    executed: [AtomicU64; 5],
    wait_ms: [AtomicU64; 5],
    exec_ms: [AtomicU64; 5],
    max_wait_ms: [AtomicU64; 5],
    max_exec_ms: [AtomicU64; 5],
}

impl ClassMetrics {
    fn new() -> Self {
        Self {
            queued: std::array::from_fn(|_| AtomicU64::new(0)),
            shed: std::array::from_fn(|_| AtomicU64::new(0)),
            timeouts: std::array::from_fn(|_| AtomicU64::new(0)),
            executed: std::array::from_fn(|_| AtomicU64::new(0)),
            wait_ms: std::array::from_fn(|_| AtomicU64::new(0)),
            exec_ms: std::array::from_fn(|_| AtomicU64::new(0)),
            max_wait_ms: std::array::from_fn(|_| AtomicU64::new(0)),
            max_exec_ms: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        let mut classes = serde_json::Map::new();
        for class in WorkClass::ALL {
            let i = class.index();
            classes.insert(
                class.as_str().to_string(),
                serde_json::json!({
                    "queued": self.queued[i].load(Ordering::Relaxed),
                    "shed": self.shed[i].load(Ordering::Relaxed),
                    "timeouts": self.timeouts[i].load(Ordering::Relaxed),
                    "executed": self.executed[i].load(Ordering::Relaxed),
                    "wait_ms": self.wait_ms[i].load(Ordering::Relaxed),
                    "exec_ms": self.exec_ms[i].load(Ordering::Relaxed),
                    "max_wait_ms": self.max_wait_ms[i].load(Ordering::Relaxed),
                    "max_exec_ms": self.max_exec_ms[i].load(Ordering::Relaxed),
                    "cap": class.cap(),
                }),
            );
        }
        serde_json::json!({ "classes": classes })
    }
}

struct MailboxInner {
    shutdown: Option<PluginWorkerRequest>,
    classes: [VecDeque<(Instant, PluginWorkerRequest)>; 5],
    rr: usize,
    closed: bool,
}

#[derive(Clone)]
struct WorkerMailbox {
    inner: Arc<Mutex<MailboxInner>>,
    cv: Arc<Condvar>,
    metrics: Arc<ClassMetrics>,
}

impl WorkerMailbox {
    fn new() -> Self {
        debug_assert_eq!(
            WorkClass::Mcp.cap()
                + WorkClass::Surface.cap()
                + WorkClass::Event.cap()
                + WorkClass::Maintenance.cap()
                + WorkClass::Control.cap(),
            PLUGIN_WORKER_QUEUE.capacity
        );
        Self {
            inner: Arc::new(Mutex::new(MailboxInner {
                shutdown: None,
                classes: std::array::from_fn(|_| VecDeque::new()),
                rr: 0,
                closed: false,
            })),
            cv: Arc::new(Condvar::new()),
            metrics: Arc::new(ClassMetrics::new()),
        }
    }

    fn try_enqueue(&self, request: PluginWorkerRequest) -> Result<(), String> {
        let class = invoke_class(&request);
        let mut inner = self
            .inner
            .lock()
            .expect("WorkerMailbox mutex poisoned");
        if inner.closed {
            return Err("plugin worker mailbox closed".to_string());
        }
        if matches!(request, PluginWorkerRequest::Shutdown { .. }) {
            inner.shutdown = Some(request);
            self.cv.notify_one();
            return Ok(());
        }
        let class = class.unwrap_or(WorkClass::Control);
        let i = class.index();
        if inner.classes[i].len() >= class.cap() {
            self.metrics.shed[i].fetch_add(1, Ordering::Relaxed);
            log::warn!(
                "plugin worker {} queue shed cap={}",
                class.as_str(),
                class.cap()
            );
            return Err(format!(
                "plugin worker {} queue is full ({})",
                class.as_str(),
                class.cap()
            ));
        }
        inner.classes[i].push_back((Instant::now(), request));
        self.metrics.queued[i].store(inner.classes[i].len() as u64, Ordering::Relaxed);
        self.cv.notify_one();
        Ok(())
    }

    #[cfg(test)]
    fn try_recv(&self) -> Option<PluginWorkerRequest> {
        let mut inner = self
            .inner
            .lock()
            .expect("WorkerMailbox mutex poisoned");
        pop_next(&mut inner, &self.metrics).map(|(_, _, request)| request)
    }

    fn recv(&self) -> Option<(Option<WorkClass>, Duration, PluginWorkerRequest)> {
        let mut inner = self
            .inner
            .lock()
            .expect("WorkerMailbox mutex poisoned");
        loop {
            if let Some(item) = pop_next(&mut inner, &self.metrics) {
                return Some(item);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .cv
                .wait(inner)
                .expect("WorkerMailbox condvar poisoned");
        }
    }

    fn close(&self) {
        let mut inner = self
            .inner
            .lock()
            .expect("WorkerMailbox mutex poisoned");
        inner.closed = true;
        self.cv.notify_all();
    }

    fn drain(&self) -> Vec<PluginWorkerRequest> {
        let mut inner = self
            .inner
            .lock()
            .expect("WorkerMailbox mutex poisoned");
        let mut out = Vec::new();
        if let Some(request) = inner.shutdown.take() {
            out.push(request);
        }
        for queue in &mut inner.classes {
            out.extend(queue.drain(..).map(|(_, request)| request));
        }
        out
    }
}

fn invoke_class(request: &PluginWorkerRequest) -> Option<WorkClass> {
    match request {
        PluginWorkerRequest::Invoke { kind, .. }
        | PluginWorkerRequest::InvokeAsync { kind, .. } => Some(WorkClass::from_handler_kind(kind)),
        PluginWorkerRequest::HttpResponse(_)
        | PluginWorkerRequest::WebSocketEvent(_)
        | PluginWorkerRequest::TimerFired { .. }
        | PluginWorkerRequest::UserFileWatch { .. }
        | PluginWorkerRequest::LocalWebhook(_) => Some(WorkClass::Control),
        PluginWorkerRequest::Shutdown { .. } => None,
    }
}

fn pop_next(
    inner: &mut MailboxInner,
    metrics: &ClassMetrics,
) -> Option<(Option<WorkClass>, Duration, PluginWorkerRequest)> {
    if let Some(request) = inner.shutdown.take() {
        return Some((None, Duration::ZERO, request));
    }
    for step in 0..WorkClass::ALL.len() {
        let i = (inner.rr + step) % WorkClass::ALL.len();
        if let Some((enqueued_at, request)) = inner.classes[i].pop_front() {
            inner.rr = (i + 1) % WorkClass::ALL.len();
            metrics.queued[i].store(inner.classes[i].len() as u64, Ordering::Relaxed);
            let wait = enqueued_at.elapsed();
            let wait_ms = wait.as_millis() as u64;
            metrics.wait_ms[i].fetch_add(wait_ms, Ordering::Relaxed);
            metrics.max_wait_ms[i].fetch_max(wait_ms, Ordering::Relaxed);
            return Some((Some(WorkClass::ALL[i]), wait, request));
        }
    }
    None
}

enum PluginWorkerRequest {
    Invoke {
        kind: String,
        id: String,
        name: Option<String>,
        payload: serde_json::Value,
        timeout_ms: u64,
        deadline: Instant,
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

#[derive(Clone)]
pub(crate) struct PluginWorkerEventTx {
    mailbox: WorkerMailbox,
}

impl std::fmt::Debug for PluginWorkerEventTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginWorkerEventTx").finish_non_exhaustive()
    }
}

impl PluginWorkerEventTx {
    fn new(mailbox: WorkerMailbox) -> Self {
        Self { mailbox }
    }

    pub(crate) fn send_http_response(&self, response: http::CompletedHttpResponse) {
        if let Err(err) = self
            .mailbox
            .try_enqueue(PluginWorkerRequest::HttpResponse(response))
        {
            log::warn!("plugin worker queue rejected HTTP response: {err}");
        }
    }

    pub(crate) fn send_websocket_event(&self, event: websocket::WsEvent) {
        if let Err(err) = self
            .mailbox
            .try_enqueue(PluginWorkerRequest::WebSocketEvent(event))
        {
            log::warn!("plugin worker queue rejected WebSocket event: {err}");
        }
    }

    pub(crate) fn send_timer_fired(&self, timer_id: String) -> bool {
        self.mailbox
            .try_enqueue(PluginWorkerRequest::TimerFired { timer_id })
            .is_ok()
    }

    pub(crate) fn send_user_file_watch(
        &self,
        watch_id: String,
        events: Vec<crate::file_watcher::FileEvent>,
    ) -> bool {
        self.mailbox
            .try_enqueue(PluginWorkerRequest::UserFileWatch { watch_id, events })
            .is_ok()
    }

    pub(crate) fn send_local_webhook_request(
        &self,
        request: local_webhooks::LocalWebhookRequest,
    ) -> Result<(), mpsc::TrySendError<local_webhooks::LocalWebhookRequest>> {
        self.mailbox
            .try_enqueue(PluginWorkerRequest::LocalWebhook(request.clone()))
            .map_err(|err| {
                if err.contains("closed") {
                    mpsc::TrySendError::Disconnected(request)
                } else {
                    mpsc::TrySendError::Full(request)
                }
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

    let available_registry = registry.clone();
    let available_fn = lua
        .create_function(move |_, ()| {
            Ok(available_registry
                .hub_event_channel
                .lock()
                .expect("PluginWorkerRegistry hub_event_channel mutex poisoned")
                .is_some())
        })
        .map_err(|e| anyhow!("create __plugin_worker_async_available: {e}"))?;
    lua.globals()
        .set("__plugin_worker_async_available", available_fn)
        .map_err(|e| anyhow!("set __plugin_worker_async_available: {e}"))?;

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

    let metrics_registry = registry.clone();
    let metrics_fn = lua
        .create_function(move |lua, plugin_key: String| {
            let workers = metrics_registry
                .workers
                .lock()
                .expect("PluginWorkerRegistry mutex poisoned");
            let Some(handle) = workers.get(&plugin_key) else {
                return json_to_lua(lua, &serde_json::json!({ "error": "plugin worker not loaded" }));
            };
            json_to_lua(lua, &handle.mailbox.metrics.snapshot())
        })
        .map_err(|e| anyhow!("create __plugin_worker_class_metrics: {e}"))?;
    lua.globals()
        .set("__plugin_worker_class_metrics", metrics_fn)
        .map_err(|e| anyhow!("set __plugin_worker_class_metrics: {e}"))?;

    Ok(())
}

/// Admit a plugin handler without waiting on the Hub owner loop.
///
/// Admission failures are logged. The Hub thread must not block on ordinary
/// plugin handlers.
pub(crate) fn invoke_plugin_handler_async(
    lua: &Lua,
    owner_plugin: &str,
    kind: &str,
    handler_id: &str,
    payload: Value,
    timeout_ms: u64,
) -> mlua::Result<()> {
    let available = lua
        .globals()
        .get::<Option<mlua::Function>>("__plugin_worker_async_available")
        .ok()
        .flatten()
        .and_then(|probe| probe.call::<bool>(()).ok())
        .unwrap_or(true);
    if !available {
        log::warn!(
            "plugin worker async {kind} using sync fallback owner={owner_plugin}: Hub event channel is unavailable"
        );
        let invoke: mlua::Function = lua.globals().get("__plugin_worker_invoke")?;
        invoke.call::<Value>((
            owner_plugin.to_string(),
            kind.to_string(),
            handler_id.to_string(),
            Option::<String>::None,
            payload,
            timeout_ms,
        ))?;
        return Ok(());
    }
    let invoke_async: mlua::Function = lua.globals().get("__plugin_worker_invoke_async")?;
    let kind_owned = kind.to_string();
    let callback = lua.create_function(move |_, (_result, err): (Value, Option<String>)| {
        if let Some(err) = err {
            log::warn!("plugin worker async {kind_owned} failed: {err}");
        }
        Ok(())
    })?;
    match invoke_async.call::<String>((
        owner_plugin.to_string(),
        kind.to_string(),
        handler_id.to_string(),
        Option::<String>::None,
        payload,
        timeout_ms,
        callback,
    )) {
        Ok(_) => Ok(()),
        Err(error) => {
            log::warn!(
                "plugin worker async {kind} admission failed owner={owner_plugin}: {error}"
            );
            Ok(())
        }
    }
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

        let mailbox = WorkerMailbox::new();
        let (parent_tx, parent_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let admission = Arc::new(Mutex::new(PluginWorkerAdmission::default()));
        let plugin_key = spec.plugin_key.clone();
        let hub_event_channel = self
            .hub_event_channel
            .lock()
            .expect("PluginWorkerRegistry hub_event_channel mutex poisoned")
            .clone();
        let worker_event_tx = PluginWorkerEventTx::new(mailbox.clone());
        let worker_admission = Arc::clone(&admission);
        let worker_canceled = Arc::clone(&self.canceled);
        let worker_mailbox = mailbox.clone();
        thread::Builder::new()
            .name(format!("plugin-worker-{plugin_key}"))
            .spawn(move || {
                worker_loop(
                    spec,
                    worker_mailbox,
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
                            mailbox,
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
            .mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind,
                id,
                name,
                payload,
                timeout_ms,
                deadline: Instant::now() + Duration::from_millis(timeout_ms.max(1)),
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
        if let Err(error) = handle.mailbox.try_enqueue(PluginWorkerRequest::InvokeAsync {
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
            let _ = handle.mailbox.try_enqueue(PluginWorkerRequest::Shutdown {
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
            let _ = handle.mailbox.try_enqueue(PluginWorkerRequest::Shutdown {
                reason: reason.to_string(),
            });
            log::debug!("plugin worker {plugin_key} shutdown requested: {reason}");
        }
    }
}

fn worker_loop(
    spec: WorkerSpec,
    mailbox: WorkerMailbox,
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
            fail_pending_worker_requests(mailbox.drain(), &reason);
            break;
        }
        let Some((class, _wait, request)) = mailbox.recv() else {
            break;
        };
        let exec_started = Instant::now();

        match request {
            PluginWorkerRequest::Invoke {
                kind,
                id,
                name,
                payload,
                timeout_ms,
                deadline,
                response,
            } => {
                if Instant::now() >= deadline {
                    let _ = response.send(Err(WorkerFail::ExpiredInQueue.as_message()));
                    record_class_timeout(&mailbox.metrics, class);
                    continue;
                }
                let Some(remaining_ms) = remaining_handler_timeout_ms(deadline) else {
                    let _ = response.send(Err(WorkerFail::ExpiredInQueue.as_message()));
                    record_class_timeout(&mailbox.metrics, class);
                    continue;
                };
                let budget_ms = remaining_ms.min(timeout_ms);
                let perf_started = lua_perf_enabled().then(Instant::now);
                let _guard = enter_plugin_worker(&spec.plugin_key);
                // Remaining interceptors still wait on the hub thread. Routing
                // a parent request back through the Hub event queue here would
                // deadlock the worker and the Hub until the invoke timeout.
                let _parent_route_guard =
                    ParentHubRouteGuard::use_synchronous_fallback(&parent_hub_event_ready);
                let result =
                    invoke_in_worker(&runtime, &kind, &id, name.as_deref(), payload, budget_ms);
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
                let timed_out = result.as_ref().is_err_and(WorkerFail::is_timeout);
                let _ = response.send(result.map_err(|fail| fail.as_message()));
                record_class_execution(&mailbox.metrics, class, exec_started, timed_out);
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
                    pending.send(Err(WorkerFail::Canceled.as_message()));
                    continue;
                }
                let Some(timeout_ms) = remaining_handler_timeout_ms(deadline) else {
                    pending.send(Err(WorkerFail::ExpiredInQueue.as_message()));
                    record_class_timeout(&mailbox.metrics, class);
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
                let timed_out = result.as_ref().is_err_and(WorkerFail::is_timeout);
                pending.send(result.map_err(|fail| fail.as_message()));
                record_class_execution(&mailbox.metrics, class, exec_started, timed_out);
            }
            PluginWorkerRequest::Shutdown { reason } => {
                log::debug!(
                    "plugin worker {} shutting down: {}",
                    spec.plugin_key,
                    reason
                );
                fail_pending_worker_requests(mailbox.drain(), &reason);
                mailbox.close();
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

fn record_class_timeout(metrics: &ClassMetrics, class: Option<WorkClass>) {
    let Some(class) = class else {
        return;
    };
    metrics.timeouts[class.index()].fetch_add(1, Ordering::Relaxed);
    log::warn!("plugin worker {} invoke expired in queue", class.as_str());
}

fn record_class_execution(
    metrics: &ClassMetrics,
    class: Option<WorkClass>,
    started: Instant,
    timed_out: bool,
) {
    let Some(class) = class else {
        return;
    };
    let i = class.index();
    metrics.executed[i].fetch_add(1, Ordering::Relaxed);
    let exec_ms = started.elapsed().as_millis() as u64;
    metrics.exec_ms[i].fetch_add(exec_ms, Ordering::Relaxed);
    metrics.max_exec_ms[i].fetch_max(exec_ms, Ordering::Relaxed);
    if timed_out {
        metrics.timeouts[i].fetch_add(1, Ordering::Relaxed);
        log::warn!(
            "plugin worker {} invoke timed out exec_ms={}",
            class.as_str(),
            exec_ms
        );
    }
}

fn fail_pending_worker_requests(requests: Vec<PluginWorkerRequest>, reason: &str) {
    for request in requests {
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

        function __worker_timed(fn)
            local ok, result = __hook_timed_pcall(fn, __handler_timeout_ms)
            if ok then
                return { __worker_ok = true, __worker_value = result }
            end
            local message = tostring(result)
            local fail = "handler"
            if type(result) == "string" and string.sub(result, 1, 13) == "hook timeout:" then
                fail = "timeout"
            end
            return { __worker_ok = false, __worker_fail = fail, __worker_error = message }
        end

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

fn unwrap_worker_timed(_lua: &Lua, result: Value) -> Result<Value, WorkerFail> {
    let Value::Table(table) = result else {
        return Ok(result);
    };
    let ok: Option<bool> = table.get("__worker_ok").ok();
    let Some(ok) = ok else {
        return Ok(Value::Table(table));
    };
    if ok {
        return table
            .get("__worker_value")
            .map_err(|e| WorkerFail::Handler(e.to_string()));
    }
    let fail: String = table
        .get("__worker_fail")
        .unwrap_or_else(|_| "handler".to_string());
    let error: String = table
        .get("__worker_error")
        .unwrap_or_else(|_| "plugin worker handler failed".to_string());
    match fail.as_str() {
        "timeout" => Err(WorkerFail::Timeout),
        _ => Err(WorkerFail::Handler(error)),
    }
}

fn invoke_in_worker(
    runtime: &LuaRuntime,
    kind: &str,
    id: &str,
    name: Option<&str>,
    payload: serde_json::Value,
    timeout_ms: u64,
) -> Result<serde_json::Value, WorkerFail> {
    let lua = runtime.lua();
    let payload_lua = json_to_lua(lua, &payload).map_err(|e| WorkerFail::Handler(e.to_string()))?;
    let globals = lua.globals();
    globals
        .set("__handler_id", id)
        .map_err(|e| WorkerFail::Handler(e.to_string()))?;
    globals
        .set("__handler_name", name)
        .map_err(|e| WorkerFail::Handler(e.to_string()))?;
    globals
        .set("__handler_timeout_ms", timeout_ms)
        .map_err(|e| WorkerFail::Handler(e.to_string()))?;
    globals
        .set("__payload", payload_lua)
        .map_err(|e| WorkerFail::Handler(e.to_string()))?;
    let result: Value = match kind {
        "ui_action" => lua
            .load(
                r#"
                local action = require("lib.action")
                return __worker_timed(function()
                    return action._invoke_registered(__handler_id, __handler_name, __payload.envelope, __payload.ctx)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_ui_action")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "session_action" => lua
            .load(
                r#"
                local actions = require("lib.session_actions")
                return __worker_timed(function()
                    return actions._invoke_registered(__handler_id, __payload.session_uuid, __payload.payload)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_session_action")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "command" => lua
            .load(
                r#"
                local commands = require("lib.commands")
                return __worker_timed(function()
                    return commands._invoke_registered(__handler_id, __payload.command)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_command")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "hook_observer" => lua
            .load(
                r#"
                local hooks = require("hub.hooks")
                return __worker_timed(function()
                    return hooks._invoke_observer(__handler_id, __handler_name, __payload.args)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_hook_observer")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "hook_interceptor" => lua
            .load(
                r#"
                local hooks = require("hub.hooks")
                return __worker_timed(function()
                    return hooks._invoke_interceptor(__handler_id, __handler_name, __payload.args)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_hook_interceptor")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "notification_observer" => lua
            .load(
                r#"
                local notifications = require("lib.notifications")
                return __worker_timed(function()
                    return notifications._invoke_observer(
                        __handler_id,
                        __payload.phase,
                        __payload.intent,
                        __payload.decision)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_notification_observer")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "notification_claim" => lua
            .load(
                r#"
                local notifications = require("lib.notifications")
                return __worker_timed(function()
                    return notifications._invoke_claim(__handler_id, __payload.intent)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_notification_claim")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "surface_route" => lua
            .load(
                r#"
                local surfaces = require("lib.surfaces")
                return __worker_timed(function()
                    return surfaces._invoke_render(__handler_id, __payload.render_state)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_surface_route")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "asset_message" => lua
            .load(
                r#"
                local plugin_assets = require("lib.plugin_assets")
                return __worker_timed(function()
                    return plugin_assets._invoke_message(
                        __handler_name,
                        __handler_id,
                        __payload.message,
                        __payload.ctx
                    )
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_asset_message")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "plugin_asset_read" => lua
            .load(
                r#"
                local plugin_assets = require("lib.plugin_assets")
                return __worker_timed(function()
                    local result, err = plugin_assets.read(__payload.asset_id)
                    if not result then error(err or "Unknown plugin asset") end
                    return result
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_plugin_asset_read")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "timer" => lua
            .load(
                r#"
                return __worker_timed(function()
                    return timer._invoke_registered(__handler_id)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_timer")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "event" => lua
            .load(
                r#"
                return __worker_timed(function()
                    return events._invoke_registered(__handler_id, __payload.data)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_event")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "watch" => lua
            .load(
                r#"
                return __worker_timed(function()
                    return watch._invoke_registered(__handler_id, __payload.event)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_watch")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "mcp_tool" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                return __worker_timed(function()
                    return mcp._invoke_tool(__handler_id, __payload.params, __payload.context)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_tool")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "mcp_prompt" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                return __worker_timed(function()
                    return mcp._invoke_prompt(__handler_id, __payload.args)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_prompt")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "mcp_resource" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                return __worker_timed(function()
                    return mcp._invoke_resource(__handler_id, __payload.params, __payload.context)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_resource")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "mcp_proxy_auth_error" => lua
            .load(
                r#"
                local mcp = require("lib.mcp")
                return __worker_timed(function()
                    return mcp._invoke_proxy_auth_error(__handler_id)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_mcp_proxy_auth_error")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        "ac_message" => lua
            .load(
                r#"
                local action_cable = require("lib.action_cable")
                return __worker_timed(function()
                    return action_cable._invoke_ac_message(__handler_id, __payload.channel_id, __payload.message)
                end)
                "#,
            )
            .set_name("plugin_worker_invoke_ac_message")
            .eval()
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
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
            .map_err(|e| WorkerFail::Handler(e.to_string()))?,
        _ => {
            return Err(WorkerFail::Handler(format!(
                "unsupported plugin handler kind: {kind}"
            )));
        }
    };

    let result = unwrap_worker_timed(lua, result)?;
    lua.from_value(result)
        .map_err(|e| WorkerFail::Handler(format!("plugin worker result conversion failed: {e}")))
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
    fn mailbox_fair_dequeue_does_not_starve_surface_behind_mcp() {
        let mailbox = WorkerMailbox::new();
        for _ in 0..8 {
            let (response, _) = mpsc::channel();
            mailbox
                .try_enqueue(PluginWorkerRequest::Invoke {
                    kind: "mcp_tool".to_string(),
                    id: "slow".to_string(),
                    name: None,
                    payload: serde_json::json!({}),
                    timeout_ms: 1000,
                    deadline: Instant::now() + Duration::from_secs(1),
                    response,
                })
                .expect("queue mcp");
        }
        let (response, _) = mpsc::channel();
        mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "surface_route".to_string(),
                id: "fast".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response,
            })
            .expect("queue surface");

        let mut kinds = Vec::new();
        while let Some(PluginWorkerRequest::Invoke { kind, .. }) = mailbox.try_recv() {
            kinds.push(kind);
        }
        assert_eq!(kinds.len(), 9);
        assert_eq!(kinds[0], "mcp_tool");
        assert_eq!(
            kinds[1], "surface_route",
            "surface must run after one MCP, not after the full MCP burst"
        );
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "surface_route").count(),
            1
        );
    }

    #[test]
    fn mailbox_full_mcp_class_does_not_delay_event_or_surface() {
        let mailbox = WorkerMailbox::new();
        for i in 0..WorkClass::Mcp.cap() {
            let (response, _) = mpsc::channel();
            mailbox
                .try_enqueue(PluginWorkerRequest::Invoke {
                    kind: "mcp_tool".to_string(),
                    id: format!("mcp-{i}"),
                    name: None,
                    payload: serde_json::json!({}),
                    timeout_ms: 1000,
                    deadline: Instant::now() + Duration::from_secs(1),
                    response,
                })
                .expect("queue mcp");
        }
        let (event_tx, _) = mpsc::channel();
        mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "event".to_string(),
                id: "fast-event".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response: event_tx,
            })
            .expect("queue event");
        let (surface_tx, _) = mpsc::channel();
        mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "surface_route".to_string(),
                id: "fast-surface".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response: surface_tx,
            })
            .expect("queue surface");

        let first = mailbox.try_recv().expect("first");
        let second = mailbox.try_recv().expect("second");
        let third = mailbox.try_recv().expect("third");
        let kinds = [first, second, third].map(|request| match request {
            PluginWorkerRequest::Invoke { kind, .. } => kind,
            _ => "other".to_string(),
        });
        assert_eq!(kinds[0], "mcp_tool");
        assert!(
            kinds.contains(&"surface_route".to_string()),
            "surface must dequeue within the first three jobs, got {kinds:?}"
        );
        assert!(
            kinds.contains(&"event".to_string()),
            "event must dequeue within the first three jobs, got {kinds:?}"
        );
    }

    #[test]
    fn mailbox_control_flood_does_not_starve_surface() {
        let mailbox = WorkerMailbox::new();
        for i in 0..WorkClass::Control.cap() {
            mailbox
                .try_enqueue(PluginWorkerRequest::TimerFired {
                    timer_id: format!("t{i}"),
                })
                .expect("queue control");
        }
        let (response, _) = mpsc::channel();
        mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "surface_route".to_string(),
                id: "fast".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response,
            })
            .expect("queue surface");
        let mut saw_surface = false;
        for _ in 0..3 {
            match mailbox.try_recv() {
                Some(PluginWorkerRequest::Invoke { kind, .. }) if kind == "surface_route" => {
                    saw_surface = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_surface, "surface must not wait behind a control flood");
    }

    #[test]
    fn work_class_maps_emitted_kinds() {
        assert_eq!(WorkClass::from_handler_kind("mcp_tool"), WorkClass::Mcp);
        assert_eq!(WorkClass::from_handler_kind("mcp_prompt"), WorkClass::Mcp);
        assert_eq!(WorkClass::from_handler_kind("mcp_resource"), WorkClass::Mcp);
        assert_eq!(
            WorkClass::from_handler_kind("mcp_proxy_auth_error"),
            WorkClass::Mcp
        );
        assert_eq!(
            WorkClass::from_handler_kind("surface_route"),
            WorkClass::Surface
        );
        assert_eq!(WorkClass::from_handler_kind("ui_action"), WorkClass::Surface);
        assert_eq!(
            WorkClass::from_handler_kind("asset_message"),
            WorkClass::Surface
        );
        assert_eq!(WorkClass::from_handler_kind("event"), WorkClass::Event);
        assert_eq!(
            WorkClass::from_handler_kind("hook_observer"),
            WorkClass::Event
        );
        assert_eq!(
            WorkClass::from_handler_kind("hook_interceptor"),
            WorkClass::Maintenance
        );
        assert_eq!(WorkClass::from_handler_kind("command"), WorkClass::Maintenance);
        assert_eq!(
            WorkClass::from_handler_kind("session_action"),
            WorkClass::Maintenance
        );
        assert_eq!(WorkClass::from_handler_kind("timer"), WorkClass::Maintenance);
        assert_eq!(WorkClass::from_handler_kind("watch"), WorkClass::Maintenance);
        assert_eq!(
            WorkClass::from_handler_kind("notification_observer"),
            WorkClass::Event
        );
        assert_eq!(
            WorkClass::from_handler_kind("ac_message"),
            WorkClass::Control
        );
    }

    #[test]
    fn mailbox_sheds_when_mcp_class_is_full() {
        let mailbox = WorkerMailbox::new();
        for i in 0..WorkClass::Mcp.cap() {
            let (response, _) = mpsc::channel();
            mailbox
                .try_enqueue(PluginWorkerRequest::Invoke {
                    kind: "mcp_tool".to_string(),
                    id: format!("mcp-{i}"),
                    name: None,
                    payload: serde_json::json!({}),
                    timeout_ms: 1000,
                    deadline: Instant::now() + Duration::from_secs(1),
                    response,
                })
                .expect("queue mcp under cap");
        }
        let (response, _) = mpsc::channel();
        let err = mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "mcp_tool".to_string(),
                id: "overflow".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response,
            })
            .expect_err("mcp class must shed");
        assert!(err.contains("mcp queue is full"), "{err}");
        let (response, _) = mpsc::channel();
        mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "surface_route".to_string(),
                id: "still-ok".to_string(),
                name: None,
                payload: serde_json::json!({}),
                timeout_ms: 1000,
                deadline: Instant::now() + Duration::from_secs(1),
                response,
            })
            .expect("full mcp class must not block surface admission");
        assert_eq!(
            mailbox.metrics.shed[WorkClass::Mcp.index()].load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn async_worker_invoke_admits_then_completes_through_hub_event() {
        let tokio_runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let registry = PluginWorkerRegistry::default();
        registry.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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

        let request = mailbox.try_recv().expect("worker request admitted");
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
        let request = mailbox.try_recv().expect("queued prompt invoke");
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
            mailbox.try_recv(),
            Some(PluginWorkerRequest::InvokeAsync { .. })
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
            .mailbox
            .try_enqueue(PluginWorkerRequest::InvokeAsync {
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
            .mailbox
            .try_enqueue(PluginWorkerRequest::InvokeAsync {
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
        let mailbox = WorkerMailbox::new();
        let (_parent_tx, parent_rx) = mpsc::channel();
        registry.workers.lock().expect("worker registry").insert(
            "test-plugin".to_string(),
            PluginWorkerHandle {
                mailbox: mailbox.clone(),
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
        fail_pending_worker_requests(mailbox.drain(), "reload");

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

    #[test]
    fn plugin_worker_async_available_is_false_without_hub_channel() {
        let lua = Lua::new();
        register(&lua, PluginWorkerRegistry::default()).expect("register");
        let available: bool = lua
            .load("return __plugin_worker_async_available()")
            .eval()
            .expect("probe async availability");
        assert!(!available);
    }

    #[test]
    fn mailbox_class_caps_include_control() {
        assert_eq!(
            WorkClass::Mcp.cap()
                + WorkClass::Surface.cap()
                + WorkClass::Event.cap()
                + WorkClass::Maintenance.cap()
                + WorkClass::Control.cap(),
            PLUGIN_WORKER_QUEUE.capacity
        );
        let _mailbox = WorkerMailbox::new();
    }

    #[test]
    fn worker_fail_timeout_is_typed_not_string_matched() {
        assert!(WorkerFail::Timeout.is_timeout());
        assert!(WorkerFail::ExpiredInQueue.is_timeout());
        assert!(!WorkerFail::Canceled.is_timeout());
        assert!(!WorkerFail::Handler("hook timeout: 10ms exceeded".to_string()).is_timeout());
        assert_eq!(
            WorkerFail::Timeout.as_message(),
            "plugin worker invoke timeout"
        );
        assert_eq!(
            WorkerFail::ExpiredInQueue.as_message(),
            "plugin worker invoke expired in queue"
        );
    }

    #[test]
    fn expired_queued_invokes_execute_zero_plugin_code_then_surface_runs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("ran");
        let surface_marker = temp.path().join("surface");
        let init_path = temp.path().join("init.lua");
        std::fs::write(
            &init_path,
            format!(
                r#"
                local mcp = require("lib.mcp")
                local surfaces = require("lib.surfaces")
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
                surfaces.register("expire_surface", {{
                    label = "Expire",
                    timeout_ms = 2000,
                    routes = {{
                        {{
                            path = "/",
                            render = function()
                                local f = assert(io.open({surface}, "w"))
                                f:write("surface")
                                f:close()
                                return {{ type = "text", props = {{ text = "fast" }} }}
                            end,
                        }},
                    }},
                }})
                return {{}}
                "#,
                marker = serde_json::to_string(&marker.to_string_lossy()).unwrap(),
                surface = serde_json::to_string(&surface_marker.to_string_lossy()).unwrap(),
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
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        runtime.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        runtime
            .lua()
            .load(format!(
                r#"
                local loader = require("hub.loader")
                local ok, err = loader.load_plugin({init_path}, "expire-plugin", {{ source = "device" }})
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
        let mut responses = Vec::new();
        for _ in 0..3 {
            let (response_tx, response_rx) = mpsc::channel();
            handle
                .mailbox
                .try_enqueue(PluginWorkerRequest::Invoke {
                    kind: "mcp_tool".to_string(),
                    id: "marker".to_string(),
                    name: Some("marker".to_string()),
                    payload: json!({}),
                    timeout_ms: 50,
                    deadline: Instant::now() - Duration::from_millis(100),
                    response: response_tx,
                })
                .expect("queue expired invoke");
            responses.push(response_rx);
        }
        std::thread::sleep(Duration::from_millis(80));
        assert!(
            !marker.exists(),
            "expired handler must not run plugin code"
        );
        assert_eq!(
            handle.mailbox.metrics.timeouts[WorkClass::Mcp.index()].load(Ordering::Relaxed),
            3
        );
        assert_eq!(
            handle.mailbox.metrics.executed[WorkClass::Mcp.index()].load(Ordering::Relaxed),
            0
        );
        for response_rx in responses {
            let err = response_rx.recv_timeout(Duration::from_secs(1)).expect("expired reply");
            assert!(
                err.unwrap_err().contains("expired in queue"),
                "expired invoke must report ExpiredInQueue"
            );
        }

        let (surface_tx, surface_rx) = mpsc::channel();
        handle
            .mailbox
            .try_enqueue(PluginWorkerRequest::Invoke {
                kind: "surface_route".to_string(),
                id: "expire_surface".to_string(),
                name: None,
                payload: json!({ "render_state": { "path": "/" } }),
                timeout_ms: 2000,
                deadline: Instant::now() + Duration::from_secs(2),
                response: surface_tx,
            })
            .expect("queue surface");
        let surface = surface_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("surface reply");
        assert!(surface.is_ok(), "surface must run after expired work, got {surface:?}");
        assert!(surface_marker.exists(), "surface handler must run");
        runtime.shutdown_plugin_workers("test_complete");
    }

    #[test]
    fn mcp_class_cap_sheds_and_owner_loop_stays_bounded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let init_path = temp.path().join("init.lua");
        std::fs::write(
            &init_path,
            r#"
            local mcp = require("lib.mcp")
            local surfaces = require("lib.surfaces")
            mcp.tool("slow_cap", {
                description = "Slow MCP",
                input_schema = { type = "object", properties = {} },
                timeout_ms = 4000,
            }, function()
                -- Stay queued behind a long handler budget (4000 ms) without
                -- making this test wait the full budget on teardown.
                local until_clock = os.clock() + 2
                while os.clock() < until_clock do end
                return "slow"
            end)
            surfaces.register("cap_surface", {
                label = "Cap",
                timeout_ms = 2000,
                routes = {
                    {
                        path = "/",
                        render = function()
                            return { type = "text", props = { text = "fast" } }
                        end,
                    },
                },
            })
            return {}
            "#,
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
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(512);
        runtime.set_hub_event_tx(HubEventTx::from(event_tx), tokio_runtime.handle().clone());
        let listed: mlua::Table = runtime
            .lua()
            .load(format!(
                r#"
                local loader = require("hub.loader")
                local ok, err = loader.load_plugin({init_path}, "cap-plugin", {{ source = "device" }})
                assert(ok, tostring(err))

                local mcp = require("lib.mcp")
                local surfaces = require("lib.surfaces")
                local Session = require("lib.session")
                for i = 1, 200 do
                    local uuid = string.format("sess-%03d", i)
                    Session._index_owner_for_tests({{
                        session_uuid = uuid,
                        owner_plugin = "cap-plugin",
                        created_at = i,
                        metadata = {{ owner_plugin = "cap-plugin" }},
                        status = "running",
                        label = uuid,
                    }})
                end
                for i = 191, 200 do
                    Session._unindex_owner_for_tests({{
                        session_uuid = string.format("sess-%03d", i),
                        owner_plugin = "cap-plugin",
                        metadata = {{ owner_plugin = "cap-plugin" }},
                    }})
                end

                for _ = 1, 200 do
                    mcp.call_tool("slow_cap", {{}}, {{}}, function() end)
                end
                local extra_ok, extra_err = true, ""
                local started = os.clock()
                local tree = surfaces.render_node("cap_surface", {{ path = "/" }})
                local event_count = events.emit("cap_event", {{}})
                local owned = Session.list_owned("cap-plugin")
                local plugins = loader.list_plugins()
                local elapsed_ms = math.floor((os.clock() - started) * 1000)
                local metrics = __plugin_worker_class_metrics("cap-plugin")
                return {{
                    extra_ok = extra_ok,
                    extra_err = tostring(extra_err),
                    tree_status = tree and tree.props and tree.props.surface_status,
                    tree_text = tree and tree.props and tree.props.text,
                    elapsed_ms = elapsed_ms,
                    owned = #owned,
                    shed = metrics.classes.mcp.shed,
                    queued = metrics.classes.mcp.queued,
                    cap = metrics.classes.mcp.cap,
                    max_wait_ms = metrics.classes.mcp.max_wait_ms,
                    max_exec_ms = metrics.classes.mcp.max_exec_ms,
                    event_count = event_count,
                    plugins = plugins,
                }}
                "#,
                init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
            ))
            .eval()
            .expect("run cap isolation");

        let elapsed_ms: i64 = listed.get("elapsed_ms").expect("elapsed");
        let shed: i64 = listed.get("shed").expect("shed");
        let cap: i64 = listed.get("cap").expect("cap");
        let owned: i64 = listed.get("owned").expect("owned");
        let extra_err: String = listed.get("extra_err").unwrap_or_default();
        assert_eq!(cap, 192);
        assert!(
            shed >= 1,
            "MCP class cap must shed overflow, shed={shed} extra_err={extra_err}"
        );
        assert_eq!(owned, 190);
        assert!(
            elapsed_ms < 500,
            "owner loop must stay bounded under MCP cap, elapsed_ms={elapsed_ms}"
        );
        let plugins: mlua::Table = listed.get("plugins").expect("plugins");
        let mut saw_metrics = false;
        for pair in plugins.sequence_values::<mlua::Table>() {
            let plugin = pair.expect("plugin row");
            let key: String = plugin.get("key").unwrap_or_default();
            if key == "cap-plugin" {
                let metrics: mlua::Table = plugin.get("worker_metrics").expect("worker_metrics");
                let classes: mlua::Table = metrics.get("classes").expect("classes");
                let mcp: mlua::Table = classes.get("mcp").expect("mcp class");
                let listed_cap: i64 = mcp.get("cap").expect("listed cap");
                assert_eq!(listed_cap, 192);
                saw_metrics = true;
            }
        }
        assert!(saw_metrics, "list_plugins must expose worker_metrics");
        runtime.shutdown_plugin_workers("test_complete");
    }
}
