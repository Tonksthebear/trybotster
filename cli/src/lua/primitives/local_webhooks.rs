//! Local webhook listener primitive.
//!
//! Provides a provider-neutral localhost HTTP listener that routes bounded
//! request payloads into plugin-owned Lua handlers. The listener owns only
//! transport concerns; provider verification and durable policy belong in
//! plugins.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Table, Value};
use rand::RngCore;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::lua::primitives::json::json_to_lua;
use crate::lua::primitives::plugin_worker::PluginWorkerEventTx;

const DEFAULT_BODY_LIMIT: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const TOKEN_BYTES: usize = 16;

#[derive(Clone, Debug)]
pub(crate) struct LocalWebhookRequest {
    pub(crate) route_id: String,
    pub(crate) generation: u64,
    pub(crate) payload: serde_json::Value,
    pub(crate) response: mpsc::Sender<std::result::Result<serde_json::Value, String>>,
}

#[derive(Debug)]
pub(crate) struct LocalWebhookResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct LocalWebhookRegistry {
    inner: Arc<Mutex<LocalWebhookState>>,
}

#[derive(Default)]
struct LocalWebhookState {
    routes: HashMap<String, RouteEntry>,
    callbacks: HashMap<String, RegistryKey>,
    next_generation: u64,
    listener: Option<ListenerHandle>,
    worker_event_tx: Option<PluginWorkerEventTx>,
}

struct ListenerHandle {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

#[derive(Clone, Debug)]
struct RouteEntry {
    id: String,
    path: String,
    token: String,
    methods: HashSet<Method>,
    body_limit: usize,
    timeout_ms: u64,
    response_mode: ResponseMode,
    owner_plugin: Option<String>,
    generation: u64,
    worker_event_tx: Option<PluginWorkerEventTx>,
}

#[derive(Clone, Debug)]
enum ResponseMode {
    Handler,
    Ack,
    Static {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
}

#[must_use]
pub(crate) fn new_local_webhook_registry() -> LocalWebhookRegistry {
    LocalWebhookRegistry {
        inner: Arc::new(Mutex::new(LocalWebhookState::default())),
    }
}

impl std::fmt::Debug for LocalWebhookRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.lock().map_err(|_| std::fmt::Error)?;
        f.debug_struct("LocalWebhookRegistry")
            .field("routes", &state.routes.len())
            .field("listener", &state.listener.as_ref().map(|l| l.addr))
            .finish()
    }
}

impl LocalWebhookRegistry {
    pub(crate) fn set_plugin_worker_event_tx(&self, tx: PluginWorkerEventTx) {
        self.inner
            .lock()
            .expect("LocalWebhookRegistry mutex poisoned")
            .worker_event_tx = Some(tx);
    }

    pub(crate) fn stop_all(&self) {
        let mut state = self
            .inner
            .lock()
            .expect("LocalWebhookRegistry mutex poisoned");
        state.routes.clear();
        state.callbacks.clear();
        if let Some(mut listener) = state.listener.take() {
            if let Some(shutdown) = listener.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }
}

pub(crate) fn register(lua: &Lua, registry: LocalWebhookRegistry) -> Result<()> {
    let table = lua
        .create_table()
        .map_err(|e| anyhow!("create local_webhooks table: {e}"))?;

    let register_registry = registry.clone();
    let register_fn = lua
        .create_function(move |lua, (opts, callback): (Table, Option<Function>)| {
            register_route(lua, &register_registry, opts, callback).map_err(mlua::Error::external)
        })
        .map_err(|e| anyhow!("create local_webhooks.register: {e}"))?;
    table
        .set("register", register_fn)
        .map_err(|e| anyhow!("set local_webhooks.register: {e}"))?;

    let unregister_registry = registry.clone();
    let unregister_fn = lua
        .create_function(move |lua, id: String| {
            unregister_route(lua, &unregister_registry, &id).map_err(mlua::Error::external)
        })
        .map_err(|e| anyhow!("create local_webhooks.unregister: {e}"))?;
    table
        .set("unregister", unregister_fn)
        .map_err(|e| anyhow!("set local_webhooks.unregister: {e}"))?;

    let cleanup_registry = registry.clone();
    let cleanup_fn = lua
        .create_function(move |lua, plugin_key: String| {
            unregister_by_plugin(lua, &cleanup_registry, &plugin_key).map_err(mlua::Error::external)
        })
        .map_err(|e| anyhow!("create local_webhooks._unregister_by_plugin: {e}"))?;
    table
        .set("_unregister_by_plugin", cleanup_fn)
        .map_err(|e| anyhow!("set local_webhooks._unregister_by_plugin: {e}"))?;

    lua.globals()
        .set("local_webhooks", table)
        .map_err(|e| anyhow!("set local_webhooks global: {e}"))?;
    Ok(())
}

fn register_route(
    lua: &Lua,
    registry: &LocalWebhookRegistry,
    opts: Table,
    callback: Option<Function>,
) -> Result<Table> {
    let id: String = opts
        .get("id")
        .map_err(|_| anyhow!("local_webhooks.register: id is required"))?;
    if id.trim().is_empty() {
        return Err(anyhow!("local_webhooks.register: id is required"));
    }

    let owner_plugin = current_plugin_key(lua);
    let is_worker = lua
        .globals()
        .get::<Option<bool>>("_loading_plugin_worker")
        .ok()
        .flatten()
        .unwrap_or(false);
    let token = opts
        .get::<Option<String>>("route_token")
        .ok()
        .flatten()
        .map(validate_route_token)
        .transpose()?
        .unwrap_or_else(generate_route_token);
    let path = route_path(
        opts.get::<Option<String>>("path").ok().flatten(),
        &id,
        &token,
    )?;
    let methods = parse_methods(opts.get::<Option<Table>>("methods").ok().flatten())?;
    let body_limit = opts
        .get::<Option<usize>>("body_limit")
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_BODY_LIMIT);
    let timeout_ms = opts
        .get::<Option<u64>>("timeout_ms")
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    let response_mode = parse_response_mode(&opts)?;

    match (&response_mode, callback.is_some()) {
        (ResponseMode::Handler | ResponseMode::Ack, false) => {
            return Err(anyhow!(
                "local_webhooks.register: callback is required for handler and ack response modes"
            ))
        }
        (ResponseMode::Static { .. }, true) => {
            return Err(anyhow!(
                "local_webhooks.register: callback is not used for static response mode"
            ))
        }
        _ => {}
    }

    if owner_plugin.is_some() && !is_worker {
        return registration_table(lua, &id, &path, &token, None);
    }

    let callback_key = match callback {
        Some(callback) => Some(
            lua.create_registry_value(callback)
                .map_err(|e| anyhow!("local_webhooks.register: store callback: {e}"))?,
        ),
        None => None,
    };

    let mut state = registry
        .inner
        .lock()
        .expect("LocalWebhookRegistry mutex poisoned");
    if state.routes.values().any(|route| route.path == path) {
        return Err(anyhow!(
            "local_webhooks.register: route path already registered"
        ));
    }
    if state.routes.values().any(|route| route.token == token) {
        return Err(anyhow!(
            "local_webhooks.register: route token already registered"
        ));
    }
    if state.routes.contains_key(&id) {
        return Err(anyhow!(
            "local_webhooks.register: route id already registered"
        ));
    }
    let addr = ensure_listener(&mut state, Arc::clone(&registry.inner))?;
    state.next_generation += 1;
    let generation = state.next_generation;
    let worker_event_tx = state.worker_event_tx.clone();
    state.routes.insert(
        id.clone(),
        RouteEntry {
            id: id.clone(),
            path: path.clone(),
            token: token.clone(),
            methods,
            body_limit,
            timeout_ms,
            response_mode,
            owner_plugin,
            generation,
            worker_event_tx,
        },
    );
    if let Some(key) = callback_key {
        state.callbacks.insert(id.clone(), key);
    }
    drop(state);

    registration_table(lua, &id, &path, &token, Some(addr))
}

fn unregister_route(lua: &Lua, registry: &LocalWebhookRegistry, id: &str) -> Result<bool> {
    let mut state = registry
        .inner
        .lock()
        .expect("LocalWebhookRegistry mutex poisoned");
    let removed = state.routes.remove(id).is_some();
    if let Some(key) = state.callbacks.remove(id) {
        let _ = lua.remove_registry_value(key);
    }
    stop_listener_if_idle(&mut state);
    Ok(removed)
}

fn unregister_by_plugin(
    lua: &Lua,
    registry: &LocalWebhookRegistry,
    plugin_key: &str,
) -> Result<usize> {
    let mut state = registry
        .inner
        .lock()
        .expect("LocalWebhookRegistry mutex poisoned");
    let ids: Vec<String> = state
        .routes
        .values()
        .filter(|route| route.owner_plugin.as_deref() == Some(plugin_key))
        .map(|route| route.id.clone())
        .collect();
    for id in &ids {
        state.routes.remove(id);
        if let Some(key) = state.callbacks.remove(id) {
            let _ = lua.remove_registry_value(key);
        }
    }
    stop_listener_if_idle(&mut state);
    Ok(ids.len())
}

pub(crate) fn fire_local_webhook_request(
    lua: &Lua,
    registry: &LocalWebhookRegistry,
    request: LocalWebhookRequest,
) {
    let result = invoke_local_webhook(lua, registry, &request);
    let _ = request.response.send(result);
}

fn invoke_local_webhook(
    lua: &Lua,
    registry: &LocalWebhookRegistry,
    request: &LocalWebhookRequest,
) -> Result<serde_json::Value, String> {
    let callback = {
        let state = registry
            .inner
            .lock()
            .expect("LocalWebhookRegistry mutex poisoned");
        match state.routes.get(&request.route_id) {
            Some(route) if route.generation == request.generation => {}
            _ => return Err("local webhook route generation is stale".to_string()),
        }
        let key = state
            .callbacks
            .get(&request.route_id)
            .ok_or_else(|| "local webhook callback missing".to_string())?;
        lua.registry_value::<Function>(key)
            .map_err(|e| format!("local webhook callback lookup failed: {e}"))?
    };
    let payload_lua = json_to_lua(lua, &request.payload).map_err(|e| e.to_string())?;
    let result: Value = callback.call(payload_lua).map_err(|e| e.to_string())?;
    lua.from_value(result)
        .map_err(|e| format!("local webhook response conversion failed: {e}"))
}

fn ensure_listener(
    state: &mut LocalWebhookState,
    shared: Arc<Mutex<LocalWebhookState>>,
) -> Result<SocketAddr> {
    if let Some(listener) = &state.listener {
        return Ok(listener.addr);
    }
    let (ready_tx, ready_rx) = mpsc::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    thread::Builder::new()
        .name("botster-local-webhooks".to_string())
        .spawn(move || run_listener(shared, ready_tx, shutdown_rx))
        .map_err(|e| anyhow!("spawn local webhook listener: {e}"))?;
    let addr = ready_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow!("local webhook listener did not start"))?
        .map_err(|e| anyhow!("local webhook listener failed: {e}"))?;
    state.listener = Some(ListenerHandle {
        addr,
        shutdown: Some(shutdown_tx),
    });
    Ok(addr)
}

fn stop_listener_if_idle(state: &mut LocalWebhookState) {
    if !state.routes.is_empty() {
        return;
    }
    // Clearing the listener while holding the mutex makes any later register
    // start a fresh listener; the old listener thread exits independently.
    if let Some(mut listener) = state.listener.take() {
        if let Some(shutdown) = listener.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn run_listener(
    shared: Arc<Mutex<LocalWebhookState>>,
    ready_tx: mpsc::Sender<Result<SocketAddr, String>>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = ready_tx.send(Err(format!("create runtime: {err}")));
            return;
        }
    };
    runtime.block_on(async move {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await {
            Ok(listener) => listener,
            Err(err) => {
                let _ = ready_tx.send(Err(format!("bind 127.0.0.1:0: {err}")));
                return;
            }
        };
        let addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(err) => {
                let _ = ready_tx.send(Err(format!("read listener addr: {err}")));
                return;
            }
        };
        let _ = ready_tx.send(Ok(addr));
        tokio::pin!(shutdown_rx);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, remote_addr)) = accepted else { continue };
                    let io = TokioIo::new(stream);
                    let state = Arc::clone(&shared);
                    tokio::task::spawn(async move {
                        let service = service_fn(move |request| {
                            handle_request(Arc::clone(&state), remote_addr, request)
                        });
                        if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                            log::debug!("local webhook connection failed: {err}");
                        }
                    });
                }
            }
        }
    });
}

async fn handle_request(
    shared: Arc<Mutex<LocalWebhookState>>,
    remote_addr: SocketAddr,
    request: Request<Incoming>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let response = match route_request(shared, remote_addr, request).await {
        Ok(response) => response,
        Err((status, body)) => LocalWebhookResponse {
            status: status.as_u16(),
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: body.into_bytes(),
        },
    };
    Ok(build_hyper_response(response))
}

async fn route_request(
    shared: Arc<Mutex<LocalWebhookState>>,
    remote_addr: SocketAddr,
    request: Request<Incoming>,
) -> std::result::Result<LocalWebhookResponse, (StatusCode, String)> {
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let route = {
        let state = shared.lock().expect("LocalWebhookRegistry mutex poisoned");
        match state.routes.values().find(|route| route.path == path) {
            Some(route) => route.clone(),
            None => return Err((StatusCode::NOT_FOUND, "not found".to_string())),
        }
    };
    if !route.methods.contains(&method) {
        return Err((
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed".to_string(),
        ));
    }
    if has_unsupported_transfer_encoding(request.headers()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "unsupported transfer-encoding".to_string(),
        ));
    }
    if let Some(length) = request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > route.body_limit {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, "body too large".to_string()));
        }
    }
    let (parts, body) = request.into_parts();
    let collected = Limited::new(body, route.body_limit)
        .collect()
        .await
        .map_err(|err| {
            if err.downcast_ref::<LengthLimitError>().is_some() {
                (StatusCode::PAYLOAD_TOO_LARGE, "body too large".to_string())
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    "malformed request body".to_string(),
                )
            }
        })?;
    let bytes = collected.to_bytes();
    match route.response_mode.clone() {
        ResponseMode::Static {
            status,
            headers,
            body,
        } => Ok(LocalWebhookResponse {
            status,
            headers,
            body,
        }),
        ResponseMode::Ack => {
            dispatch_to_worker(&shared, &route, &parts, remote_addr, &bytes, false).await
        }
        ResponseMode::Handler => {
            dispatch_to_worker(&shared, &route, &parts, remote_addr, &bytes, true).await
        }
    }
}

async fn dispatch_to_worker(
    shared: &Arc<Mutex<LocalWebhookState>>,
    route: &RouteEntry,
    parts: &hyper::http::request::Parts,
    remote_addr: SocketAddr,
    bytes: &[u8],
    wait: bool,
) -> std::result::Result<LocalWebhookResponse, (StatusCode, String)> {
    let Some(worker_tx) = route.worker_event_tx.clone() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook worker unavailable".to_string(),
        ));
    };
    let payload = request_payload(route, parts, remote_addr, bytes);
    let (response_tx, response_rx) = mpsc::channel();
    let request = LocalWebhookRequest {
        route_id: route.id.clone(),
        generation: route.generation,
        payload,
        response: response_tx,
    };
    worker_tx.send_local_webhook_request(request).map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook worker busy".to_string(),
        )
    })?;
    if !wait {
        return Ok(LocalWebhookResponse {
            status: StatusCode::ACCEPTED.as_u16(),
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: b"accepted".to_vec(),
        });
    }
    let timeout = Duration::from_millis(route.timeout_ms.max(1));
    let result = tokio::task::spawn_blocking(move || response_rx.recv_timeout(timeout))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "webhook handler failed".to_string(),
            )
        })?;
    let value = match result {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "webhook handler failed".to_string(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err((
                StatusCode::GATEWAY_TIMEOUT,
                "webhook handler timeout".to_string(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "webhook handler failed".to_string(),
            ))
        }
    };
    let current = {
        let state = shared.lock().expect("LocalWebhookRegistry mutex poisoned");
        state
            .routes
            .get(&route.id)
            .is_some_and(|current| current.generation == route.generation)
    };
    if !current {
        return Err((StatusCode::NOT_FOUND, "not found".to_string()));
    }
    Ok(response_from_json(value))
}

fn request_payload(
    route: &RouteEntry,
    parts: &hyper::http::request::Parts,
    remote_addr: SocketAddr,
    bytes: &[u8],
) -> serde_json::Value {
    let headers: serde_json::Map<String, serde_json::Value> = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), serde_json::json!(value)))
        })
        .collect();
    serde_json::json!({
        "request_id": format!("wh_{}", next_request_id()),
        "route_id": route.id,
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "query": parts.uri.query().unwrap_or(""),
        "headers": headers,
        "body": String::from_utf8_lossy(bytes).to_string(),
        "raw_body": String::from_utf8_lossy(bytes).to_string(),
        "body_truncated": false,
        "remote_addr": remote_addr.ip().to_string(),
        "received_at": rfc3339_now(),
    })
}

fn build_hyper_response(response: LocalWebhookResponse) -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(response.status);
    for (name, value) in response.headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Full::new(Bytes::from(response.body)))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from_static(b"webhook response error")))
                .expect("static response builds")
        })
}

fn response_from_json(value: serde_json::Value) -> LocalWebhookResponse {
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .filter(|status| (100..=599).contains(status))
        .map_or(200, |status| status as u16);
    let headers = value
        .get("headers")
        .and_then(serde_json::Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(name, value)| value.as_str().map(|v| (name.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    LocalWebhookResponse {
        status,
        headers,
        body,
    }
}

fn parse_response_mode(opts: &Table) -> Result<ResponseMode> {
    let mode = opts
        .get::<Option<String>>("response_mode")
        .ok()
        .flatten()
        .unwrap_or_else(|| "handler".to_string());
    match mode.as_str() {
        "handler" => Ok(ResponseMode::Handler),
        "ack" => Ok(ResponseMode::Ack),
        "static" => {
            let response: Option<Table> = opts.get("response").ok().flatten();
            let status = response
                .as_ref()
                .and_then(|table| table.get::<Option<u16>>("status").ok().flatten())
                .unwrap_or(StatusCode::ACCEPTED.as_u16());
            let headers = response
                .as_ref()
                .and_then(|table| table.get::<Option<Table>>("headers").ok().flatten())
                .map(table_to_headers)
                .transpose()?
                .unwrap_or_default();
            let body = response
                .as_ref()
                .and_then(|table| table.get::<Option<String>>("body").ok().flatten())
                .unwrap_or_default()
                .into_bytes();
            Ok(ResponseMode::Static {
                status,
                headers,
                body,
            })
        }
        _ => Err(anyhow!("local_webhooks.register: invalid response_mode")),
    }
}

fn parse_methods(methods: Option<Table>) -> Result<HashSet<Method>> {
    let mut parsed = HashSet::new();
    if let Some(methods) = methods {
        for value in methods.sequence_values::<String>() {
            let method = value
                .map_err(|e| anyhow!("local_webhooks.register: invalid method: {e}"))?
                .to_ascii_uppercase();
            match method.as_str() {
                "POST" => {
                    parsed.insert(Method::POST);
                }
                "PUT" => {
                    parsed.insert(Method::PUT);
                }
                _ => {
                    return Err(anyhow!(
                        "local_webhooks.register: method must be POST or PUT"
                    ))
                }
            }
        }
    } else {
        parsed.insert(Method::POST);
    }
    if parsed.is_empty() {
        return Err(anyhow!(
            "local_webhooks.register: at least one method is required"
        ));
    }
    Ok(parsed)
}

fn table_to_headers(table: Table) -> Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for pair in table.pairs::<String, String>() {
        headers.push(pair.map_err(|e| anyhow!("local_webhooks.register: invalid header: {e}"))?);
    }
    Ok(headers)
}

fn route_path(path: Option<String>, id: &str, token: &str) -> Result<String> {
    let path = path.unwrap_or_else(|| format!("/webhooks/{}/{}", sanitize_path_part(id), token));
    let path = path.replace("<route_token>", token);
    if !path.starts_with('/') || path.contains('?') {
        return Err(anyhow!(
            "local_webhooks.register: path must be an absolute path without query"
        ));
    }
    Ok(path)
}

fn validate_route_token(token: String) -> Result<String> {
    if token.len() < 22
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(anyhow!(
            "local_webhooks.register: route_token must be unguessable url-safe text"
        ));
    }
    Ok(token)
}

fn generate_route_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sanitize_path_part(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn current_plugin_key(lua: &Lua) -> Option<String> {
    let globals = lua.globals();
    globals
        .get::<Option<String>>("_plugin_worker_key")
        .ok()
        .flatten()
        .or_else(|| {
            globals
                .get::<Option<String>>("_loading_plugin_key")
                .ok()
                .flatten()
        })
        .or_else(|| {
            globals
                .get::<Option<String>>("_loading_plugin_name")
                .ok()
                .flatten()
        })
        .filter(|key| !key.is_empty())
}

fn registration_table(
    lua: &Lua,
    id: &str,
    path: &str,
    token: &str,
    addr: Option<SocketAddr>,
) -> Result<Table> {
    let table = lua
        .create_table()
        .map_err(|e| anyhow!("local_webhooks.register: create response table: {e}"))?;
    table
        .set("id", id)
        .map_err(|e| anyhow!("local_webhooks.register: set id: {e}"))?;
    table
        .set("route_id", id)
        .map_err(|e| anyhow!("local_webhooks.register: set route_id: {e}"))?;
    table
        .set("path", path)
        .map_err(|e| anyhow!("local_webhooks.register: set path: {e}"))?;
    table
        .set("route_token", token)
        .map_err(|e| anyhow!("local_webhooks.register: set route_token: {e}"))?;
    if let Some(addr) = addr {
        table
            .set("port", addr.port())
            .map_err(|e| anyhow!("local_webhooks.register: set port: {e}"))?;
        table
            .set(
                "url",
                format!("http://{}:{}{}", addr.ip(), addr.port(), path),
            )
            .map_err(|e| anyhow!("local_webhooks.register: set url: {e}"))?;
    }
    Ok(table)
}

fn has_unsupported_transfer_encoding(headers: &hyper::HeaderMap) -> bool {
    let values: Vec<String> = headers
        .get_all(hyper::header::TRANSFER_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(',').map(str::trim).map(str::to_ascii_lowercase))
        .filter(|value| !value.is_empty())
        .collect();
    values.iter().any(|value| value != "chunked") || values.len() > 1
}

fn next_request_id() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn rfc3339_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpStream};

    fn lua_with_registry() -> (Lua, LocalWebhookRegistry) {
        let lua = Lua::new();
        let registry = new_local_webhook_registry();
        register(&lua, registry.clone()).unwrap();
        (lua, registry)
    }

    fn static_route_url(lua: &Lua, script: &str) -> (String, u16) {
        let route: Table = lua.load(script).eval().unwrap();
        (route.get("url").unwrap(), route.get("port").unwrap())
    }

    fn post_text(url: &str, body: &str) -> reqwest::blocking::Response {
        reqwest::blocking::Client::new()
            .post(url)
            .body(body.to_string())
            .send()
            .unwrap()
    }

    fn raw_http(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn generated_route_token_has_at_least_128_bits_of_entropy_source() {
        let token = generate_route_token();
        assert!(token.len() >= 22, "{token}");
        assert!(token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    }

    #[test]
    fn registration_rejects_methods_outside_post_put() {
        let (lua, _registry) = lua_with_registry();
        let err = lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "bad",
                  methods = { "GET" },
                  response_mode = "static",
                })
                "#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(
            err.to_string().contains("method must be POST or PUT"),
            "{err}"
        );
    }

    #[test]
    fn static_route_accepts_registered_path_and_unknown_path_returns_404() {
        let (lua, registry) = lua_with_registry();
        let (url, port) = static_route_url(
            &lua,
            r#"
            return local_webhooks.register({
              id = "static-ok",
              path = "/webhooks/static-ok",
              response_mode = "static",
              response = {
                status = 201,
                headers = { ["x-local-webhook-test"] = "ok" },
                body = "static-body",
              },
            })
            "#,
        );

        let response = post_text(&url, "hello");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get("x-local-webhook-test").unwrap(),
            "ok"
        );
        assert_eq!(response.text().unwrap(), "static-body");

        let missing = post_text(&format!("http://127.0.0.1:{port}/missing"), "hello");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        registry.stop_all();
    }

    #[test]
    fn unregistered_method_on_registered_route_returns_405() {
        let (lua, registry) = lua_with_registry();
        let (url, _port) = static_route_url(
            &lua,
            r#"
            return local_webhooks.register({
              id = "method-filter",
              path = "/webhooks/method-filter",
              response_mode = "static",
            })
            "#,
        );

        let response = reqwest::blocking::Client::new().get(url).send().unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        registry.stop_all();
    }

    #[test]
    fn body_limits_apply_to_content_length_and_chunked_requests() {
        let (lua, registry) = lua_with_registry();
        let (url, port) = static_route_url(
            &lua,
            r#"
            return local_webhooks.register({
              id = "limited",
              path = "/webhooks/limited",
              body_limit = 4,
              response_mode = "static",
            })
            "#,
        );

        let response = post_text(&url, "12345");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let raw = raw_http(
            port,
            concat!(
                "POST /webhooks/limited HTTP/1.1\r\n",
                "Host: 127.0.0.1\r\n",
                "Transfer-Encoding: chunked\r\n",
                "\r\n",
                "3\r\nabc\r\n",
                "3\r\ndef\r\n",
                "0\r\n\r\n"
            ),
        );
        assert!(raw.starts_with("HTTP/1.1 413"), "{raw}");
        registry.stop_all();
    }

    #[test]
    fn unsupported_transfer_encoding_returns_400_before_dispatch() {
        let (lua, registry) = lua_with_registry();
        let (_url, port) = static_route_url(
            &lua,
            r#"
            return local_webhooks.register({
              id = "bad-te",
              path = "/webhooks/bad-te",
              response_mode = "static",
            })
            "#,
        );

        let raw = raw_http(
            port,
            concat!(
                "POST /webhooks/bad-te HTTP/1.1\r\n",
                "Host: 127.0.0.1\r\n",
                "Transfer-Encoding: gzip\r\n",
                "\r\n",
                "body"
            ),
        );
        assert!(raw.starts_with("HTTP/1.1 400"), "{raw}");
        registry.stop_all();
    }

    #[test]
    fn unregister_removes_route_and_collisions_are_rejected() {
        let (lua, registry) = lua_with_registry();
        let (url, _port) = static_route_url(
            &lua,
            r#"
            return local_webhooks.register({
              id = "to-remove",
              path = "/webhooks/to-remove",
              route_token = "abcdefghijklmnopqrstuv",
              response_mode = "static",
            })
            "#,
        );

        let duplicate_path = lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "duplicate-path",
                  path = "/webhooks/to-remove",
                  route_token = "abcdefghijklmnopqrstuw",
                  response_mode = "static",
                })
                "#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(
            duplicate_path
                .to_string()
                .contains("route path already registered"),
            "{duplicate_path}"
        );

        let duplicate_token = lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "duplicate-token",
                  path = "/webhooks/duplicate-token",
                  route_token = "abcdefghijklmnopqrstuv",
                  response_mode = "static",
                })
                "#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(
            duplicate_token
                .to_string()
                .contains("route token already registered"),
            "{duplicate_token}"
        );

        assert!(lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "keeper",
                  path = "/webhooks/keeper",
                  route_token = "abcdefghijklmnopqrstux",
                  response_mode = "static",
                }) ~= nil
                "#,
            )
            .eval::<bool>()
            .unwrap());
        assert!(lua
            .load(r#"return local_webhooks.unregister("to-remove")"#)
            .eval::<bool>()
            .unwrap());
        let response = post_text(&url, "hello");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        registry.stop_all();
    }

    #[test]
    fn invalid_response_mode_is_rejected() {
        let (lua, _registry) = lua_with_registry();
        let err = lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "bad-mode",
                  response_mode = "later",
                })
                "#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("invalid response_mode"), "{err}");
    }

    #[test]
    fn static_response_mode_rejects_unused_callback() {
        let (lua, _registry) = lua_with_registry();
        let err = lua
            .load(
                r#"
                return local_webhooks.register({
                  id = "static-with-callback",
                  response_mode = "static",
                }, function(_request)
                  return { status = 200 }
                end)
                "#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("callback is not used for static response mode"),
            "{err}"
        );
    }

    #[test]
    fn independent_registries_bind_concurrent_ephemeral_localhost_ports() {
        let (lua_a, registry_a) = lua_with_registry();
        let (lua_b, registry_b) = lua_with_registry();
        let (_url_a, port_a) = static_route_url(
            &lua_a,
            r#"
            return local_webhooks.register({
              id = "hub-a",
              path = "/webhooks/hub-a",
              response_mode = "static",
            })
            "#,
        );
        let (_url_b, port_b) = static_route_url(
            &lua_b,
            r#"
            return local_webhooks.register({
              id = "hub-b",
              path = "/webhooks/hub-b",
              response_mode = "static",
            })
            "#,
        );

        assert_ne!(port_a, 0);
        assert_ne!(port_b, 0);
        assert_ne!(port_a, port_b);
        registry_a.stop_all();
        registry_b.stop_all();
    }
}
