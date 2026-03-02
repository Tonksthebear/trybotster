//! HTTP client primitives for Lua scripts.
//!
//! Provides both synchronous and asynchronous HTTP request functions.
//!
//! # Synchronous API
//!
//! `http.get/post/put/delete` block until the response arrives. Safe to use
//! at init time (before the tick loop starts). When called from within an
//! active tokio runtime they use [`tokio::task::block_in_place`] to avoid
//! a "Cannot drop a runtime" panic, but they **still stall the event loop**
//! for the full round-trip. Do not use sync methods from plugin callbacks.
//!
//! # Asynchronous API
//!
//! `http.request()` spawns an isolated background thread and returns
//! immediately. The callback fires on the next Hub tick after the response
//! arrives. **This is the only safe HTTP API for plugin callbacks.**
//!
//! Two calling conventions are accepted:
//!
//! ```lua
//! -- Positional form
//! http.request("GET", "https://api.example.com/data", {}, function(resp, err)
//!     if resp then log.info(resp.body) end
//! end)
//!
//! -- Table-first form (matches mcp_defaults.lua documentation)
//! http.request({ method = "GET", url = "https://api.example.com/data" }, function(resp, err)
//!     if resp then log.info(resp.body) end
//! end)
//!
//! -- Table-first with options
//! http.request({
//!     method  = "POST",
//!     url     = "https://api.example.com/data",
//!     json    = { name = "bot", version = 1 },
//!     headers = { ["Authorization"] = "Bearer token" },
//!     timeout_ms = 35000,
//! }, function(resp, err)
//!     if resp then log.info("Created!") end
//! end)
//! ```
//!
//! # Synchronous API (init-time only)
//!
//! ```lua
//! local resp, err = http.get("https://api.example.com/data")
//! if resp then
//!     log.info("Status: " .. resp.status)
//!     log.info("Body: " .. resp.body)
//! end
//! ```
//!
//! # Error Handling
//!
//! All functions return two values following Lua convention:
//! - Success: `value, nil`
//! - Failure: `nil, error_message`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, MultiValue, Table, Value};

/// Default request timeout in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Maximum number of concurrent async HTTP requests.
/// Prevents thread exhaustion from rapid-fire `http.request()` calls.
const MAX_CONCURRENT_HTTP_REQUESTS: usize = 16;

// =============================================================================
// Async HTTP types
// =============================================================================

/// Completed HTTP response data (plain Rust types, no Lua references).
///
/// Sent through the `HubEvent::HttpResponse` channel by background threads,
/// or pushed to the shared vec in test mode.
pub(crate) struct CompletedHttpResponse {
    /// Request ID for matching against pending callbacks.
    pub(crate) request_id: String,
    /// Response payload or error message.
    pub(crate) result: std::result::Result<HttpResponseData, String>,
}

impl std::fmt::Debug for CompletedHttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletedHttpResponse")
            .field("request_id", &self.request_id)
            .field("is_ok", &self.result.is_ok())
            .finish()
    }
}

/// Successful HTTP response payload.
pub(crate) struct HttpResponseData {
    /// HTTP status code (e.g., 200, 404).
    pub(crate) status: u16,
    /// Response body text.
    pub(crate) body: String,
    /// Response headers as key-value pairs.
    pub(crate) headers: Vec<(String, String)>,
}

/// Async HTTP registry tracking in-flight requests and completed responses.
///
/// Pending callbacks are stored as `LuaRegistryKey` (main-thread only).
/// Background threads send `HubEvent::HttpResponse` via the event channel
/// (production) or push to the responses vec (tests without a channel).
pub struct HttpAsyncEntries {
    /// Callbacks awaiting responses, keyed by request_id.
    pub(crate) pending: HashMap<String, mlua::RegistryKey>,
    /// Completed responses waiting to fire callbacks (test-only fallback).
    responses: Vec<CompletedHttpResponse>,
    /// Counter for generating unique request IDs.
    next_id: u64,
    /// Number of background threads currently executing HTTP requests.
    in_flight: usize,
    /// Event channel sender for instant delivery to the Hub event loop.
    /// `None` in tests that don't wire up the full event bus.
    hub_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>,
    /// Shared HTTP client reused across all async requests.
    ///
    /// `reqwest::blocking::Client` is an `Arc` internally — cloning it is cheap
    /// and shares the underlying connection pool. Each thread receives a clone so
    /// connections to the same host (e.g. Telegram polling every 5 s) are
    /// reused instead of opening a fresh TCP socket on every request. Per-request
    /// timeouts are applied via `RequestBuilder::timeout()`, which overrides the
    /// client-level setting for that specific call.
    client: reqwest::blocking::Client,
}

impl Default for HttpAsyncEntries {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            responses: Vec::new(),
            next_id: 0,
            in_flight: 0,
            hub_event_tx: None,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl HttpAsyncEntries {
    /// Set the Hub event channel sender for event-driven delivery.
    ///
    /// When set, background threads send `HubEvent::HttpResponse` through
    /// this channel instead of pushing to the shared vec.
    pub(crate) fn set_hub_event_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
    ) {
        self.hub_event_tx = Some(tx);
    }

    /// Emit a completed response through the event channel or shared vec.
    ///
    /// If `hub_event_tx` is set (production), sends via the channel for
    /// instant delivery. Otherwise falls back to the shared vec (tests).
    fn emit_response(&mut self, response: CompletedHttpResponse) {
        if let Some(ref tx) = self.hub_event_tx {
            let _ = tx.send(crate::hub::events::HubEvent::HttpResponse(response));
        } else {
            self.responses.push(response);
        }
    }
}

impl HttpAsyncEntries {
    /// Number of requests awaiting responses.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Number of background threads currently executing HTTP requests.
    #[must_use]
    pub fn in_flight_count(&self) -> usize {
        self.in_flight
    }
}

impl std::fmt::Debug for HttpAsyncEntries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpAsyncEntries")
            .field("pending_count", &self.pending.len())
            .field("in_flight", &self.in_flight)
            .field("responses_queued", &self.responses.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

/// Thread-safe handle to the async HTTP registry.
pub type HttpAsyncRegistry = Arc<Mutex<HttpAsyncEntries>>;

/// Create a new shared async HTTP registry.
#[must_use]
pub fn new_http_registry() -> HttpAsyncRegistry {
    Arc::new(Mutex::new(HttpAsyncEntries::default()))
}

// =============================================================================
// Sync helpers
// =============================================================================

/// Build a response table from a reqwest blocking response.
///
/// Returns a Lua table with fields:
/// - `status` (integer) - HTTP status code
/// - `body` (string) - Response body text
/// - `headers` (table) - Response headers as key-value pairs
fn build_response_table(lua: &Lua, resp: reqwest::blocking::Response) -> mlua::Result<Table> {
    let status = resp.status().as_u16();

    // Collect headers before consuming the response body
    let headers_table = lua.create_table()?;
    for (name, value) in resp.headers() {
        if let Ok(v) = value.to_str() {
            headers_table.set(name.as_str().to_string(), v.to_string())?;
        }
    }

    let body = resp.text().unwrap_or_default();

    let table = lua.create_table()?;
    table.set("status", status)?;
    table.set("body", body)?;
    table.set("headers", headers_table)?;

    Ok(table)
}

/// Extract common options from an opts table.
///
/// Reads `headers`, `json`, `body`, and `timeout_ms` from the
/// optional Lua options table.
struct RequestOpts {
    headers: Vec<(String, String)>,
    json_body: Option<serde_json::Value>,
    raw_body: Option<String>,
    timeout: Duration,
}

/// Parse a Lua options table into `RequestOpts`.
fn parse_opts(lua: &Lua, opts: Option<Table>) -> mlua::Result<RequestOpts> {
    let mut headers = Vec::new();
    let mut json_body = None;
    let mut raw_body = None;
    let mut timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);

    if let Some(opts) = opts {
        // Parse headers
        if let Ok(h) = opts.get::<Table>("headers") {
            for pair in h.pairs::<String, String>() {
                let (k, v) = pair?;
                headers.push((k, v));
            }
        }

        // Parse json body
        if let Ok(json_val) = opts.get::<Value>("json") {
            if json_val != Value::Nil {
                let serde_val: serde_json::Value =
                    lua.from_value(json_val).map_err(|e| {
                        mlua::Error::external(format!("Failed to serialize json option: {e}"))
                    })?;
                json_body = Some(serde_val);
            }
        }

        // Parse raw body
        if let Ok(body) = opts.get::<String>("body") {
            raw_body = Some(body);
        }

        // Parse timeout
        if let Ok(ms) = opts.get::<u64>("timeout_ms") {
            timeout = Duration::from_millis(ms);
        }
    }

    Ok(RequestOpts {
        headers,
        json_body,
        raw_body,
        timeout,
    })
}

/// Apply headers and body to a reqwest request builder.
fn apply_opts(
    mut builder: reqwest::blocking::RequestBuilder,
    opts: &RequestOpts,
) -> reqwest::blocking::RequestBuilder {
    for (k, v) in &opts.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    if let Some(ref json) = opts.json_body {
        builder = builder.json(json);
    } else if let Some(ref body) = opts.raw_body {
        builder = builder.body(body.clone());
    }

    builder
}

/// Execute a blocking closure safely regardless of whether a tokio runtime is active.
///
/// Inside a tokio multi-threaded runtime, `reqwest::blocking` panics with
/// "Cannot drop a runtime" if its internal `current_thread` runtime is torn
/// down on a thread that already owns a runtime handle. [`tokio::task::block_in_place`]
/// signals the scheduler to evacuate other tasks off the current thread before
/// blocking, preventing the nested-runtime conflict.
///
/// Outside any tokio context (init-time scripts, unit tests) the closure is
/// called directly — `block_in_place` is not available without a runtime.
///
/// **Important:** even with `block_in_place` the calling thread is blocked for
/// the full request duration. Do not call sync HTTP methods from plugin callbacks;
/// use `http.request()` instead.
fn execute_blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

/// Register the `http` table with synchronous and asynchronous HTTP functions.
///
/// Creates a global `http` table with methods:
/// - `http.get(url, opts?)` — Sync GET; safe at init time, blocks tick loop at runtime
/// - `http.post(url, opts?)` — Sync POST; same caveat
/// - `http.put(url, opts?)` — Sync PUT; same caveat
/// - `http.delete(url, opts?)` — Sync DELETE; same caveat
/// - `http.request(...)` — Async, non-blocking; **use this from plugin callbacks**
///
/// `http.request` accepts two calling conventions:
/// - Positional:  `http.request("METHOD", "url"[, opts_table], callback)`
/// - Table-first: `http.request({method="METHOD", url="url"[, ...]}, callback)`
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, registry: HttpAsyncRegistry) -> Result<()> {
    let http_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create http table: {e}"))?;

    // One-time warning for sync HTTP usage.
    static SYNC_WARNING: std::sync::Once = std::sync::Once::new();

    // http.get(url, opts?) -> (response, nil) or (nil, error_string)
    let get_fn = lua
        .create_function(|lua, (url, opts): (String, Option<Table>)| {
            SYNC_WARNING.call_once(|| {
                log::warn!(
                    "[http] Sync http.get/post/put/delete blocks the tick loop. \
                     Use http.request() for non-blocking HTTP."
                );
            });
            let opts = parse_opts(lua, opts)?;
            let result = execute_blocking(|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(opts.timeout)
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
                apply_opts(client.get(&url), &opts)
                    .send()
                    .map_err(|e| format!("HTTP GET failed: {e}"))
            });
            match result {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(e))),
            }
        })
        .map_err(|e| anyhow!("Failed to create http.get function: {e}"))?;

    http_table
        .set("get", get_fn)
        .map_err(|e| anyhow!("Failed to set http.get: {e}"))?;

    // http.post(url, opts?) -> (response, nil) or (nil, error_string)
    let post_fn = lua
        .create_function(|lua, (url, opts): (String, Option<Table>)| {
            SYNC_WARNING.call_once(|| {
                log::warn!(
                    "[http] Sync http.get/post/put/delete blocks the tick loop. \
                     Use http.request() for non-blocking HTTP."
                );
            });
            let opts = parse_opts(lua, opts)?;
            let result = execute_blocking(|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(opts.timeout)
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
                apply_opts(client.post(&url), &opts)
                    .send()
                    .map_err(|e| format!("HTTP POST failed: {e}"))
            });
            match result {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(e))),
            }
        })
        .map_err(|e| anyhow!("Failed to create http.post function: {e}"))?;

    http_table
        .set("post", post_fn)
        .map_err(|e| anyhow!("Failed to set http.post: {e}"))?;

    // http.put(url, opts?) -> (response, nil) or (nil, error_string)
    let put_fn = lua
        .create_function(|lua, (url, opts): (String, Option<Table>)| {
            SYNC_WARNING.call_once(|| {
                log::warn!(
                    "[http] Sync http.get/post/put/delete blocks the tick loop. \
                     Use http.request() for non-blocking HTTP."
                );
            });
            let opts = parse_opts(lua, opts)?;
            let result = execute_blocking(|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(opts.timeout)
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
                apply_opts(client.put(&url), &opts)
                    .send()
                    .map_err(|e| format!("HTTP PUT failed: {e}"))
            });
            match result {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(e))),
            }
        })
        .map_err(|e| anyhow!("Failed to create http.put function: {e}"))?;

    http_table
        .set("put", put_fn)
        .map_err(|e| anyhow!("Failed to set http.put: {e}"))?;

    // http.delete(url, opts?) -> (response, nil) or (nil, error_string)
    let delete_fn = lua
        .create_function(|lua, (url, opts): (String, Option<Table>)| {
            SYNC_WARNING.call_once(|| {
                log::warn!(
                    "[http] Sync http.get/post/put/delete blocks the tick loop. \
                     Use http.request() for non-blocking HTTP."
                );
            });
            let opts = parse_opts(lua, opts)?;
            let result = execute_blocking(|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(opts.timeout)
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {e}"))?;
                apply_opts(client.delete(&url), &opts)
                    .send()
                    .map_err(|e| format!("HTTP DELETE failed: {e}"))
            });
            match result {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(e))),
            }
        })
        .map_err(|e| anyhow!("Failed to create http.delete function: {e}"))?;

    http_table
        .set("delete", delete_fn)
        .map_err(|e| anyhow!("Failed to set http.delete: {e}"))?;

    // http.request(...) -> (request_id, nil) or (nil, error)
    //
    // Async HTTP request. Spawns a background thread, returns immediately.
    // The callback fires on the next Hub tick after the response arrives:
    //   callback(response_table, nil)  -- on success
    //   callback(nil, error_string)    -- on failure
    //
    // Accepts two calling conventions:
    //   Positional:  http.request("GET", "url"[, opts_table], callback)
    //   Table-first: http.request({method="GET", url="url"[, ...]}, callback)
    //
    // Returns (nil, error_string) if the concurrency limit is reached.
    let request_fn = lua
        .create_function(
            move |lua, args: MultiValue| {
                // ── Argument parsing ─────────────────────────────────────────
                // Support both calling conventions by inspecting the first arg.
                let mut args: Vec<Value> = args.into_iter().collect();
                if args.is_empty() {
                    return Err(mlua::Error::runtime(
                        "http.request: expected at least 2 arguments",
                    ));
                }

                let (method, url, opts, callback) = match args.remove(0) {
                    // ── Table-first: http.request({method="GET", url="…"[, …]}, cb) ──
                    Value::Table(opts_table) => {
                        let method: String = opts_table.get("method").map_err(|e| {
                            mlua::Error::runtime(format!(
                                "http.request: 'method' field error: {e} (expected a string, e.g. \"GET\")"
                            ))
                        })?;
                        let url: String = opts_table.get("url").map_err(|e| {
                            mlua::Error::runtime(format!(
                                "http.request: 'url' field error: {e} (expected a string)"
                            ))
                        })?;
                        let callback = match args.into_iter().next() {
                            Some(Value::Function(f)) => f,
                            _ => {
                                return Err(mlua::Error::runtime(
                                    "http.request: expected callback function as second argument",
                                ));
                            }
                        };
                        let opts = parse_opts(lua, Some(opts_table))?;
                        (method, url, opts, callback)
                    }

                    // ── Positional: http.request("METHOD", "url"[, opts], cb) ──
                    Value::String(method_str) => {
                        let method = method_str.to_str().map_err(|_| {
                            mlua::Error::runtime("http.request: method must be a valid string")
                        })?.to_string();

                        let url = match args.first() {
                            Some(Value::String(_)) => match args.remove(0) {
                                Value::String(s) => s.to_str().map_err(|_| {
                                    mlua::Error::runtime(
                                        "http.request: url must be a valid string",
                                    )
                                })?.to_string(),
                                _ => unreachable!(),
                            },
                            _ => {
                                return Err(mlua::Error::runtime(
                                    "http.request: expected URL string as 2nd argument",
                                ));
                            }
                        };

                        // Remaining args: [opts_table?, callback]
                        let (opts_table, callback) = match args.len() {
                            0 => {
                                return Err(mlua::Error::runtime(
                                    "http.request: expected callback function",
                                ));
                            }
                            1 => {
                                // 3-arg form: method, url, callback (no opts)
                                match args.remove(0) {
                                    Value::Function(f) => (None, f),
                                    _ => {
                                        return Err(mlua::Error::runtime(
                                            "http.request: 3rd argument must be a callback function",
                                        ));
                                    }
                                }
                            }
                            _ => {
                                // 4-arg form: method, url, opts, callback
                                let opts_table = match args.remove(0) {
                                    Value::Table(t) => Some(t),
                                    Value::Nil => None,
                                    _ => {
                                        return Err(mlua::Error::runtime(
                                            "http.request: 3rd argument must be an opts table or nil",
                                        ));
                                    }
                                };
                                let callback = match args.remove(0) {
                                    Value::Function(f) => f,
                                    _ => {
                                        return Err(mlua::Error::runtime(
                                            "http.request: 4th argument must be a callback function",
                                        ));
                                    }
                                };
                                (opts_table, callback)
                            }
                        };

                        let opts = parse_opts(lua, opts_table)?;
                        (method, url, opts, callback)
                    }

                    _ => {
                        return Err(mlua::Error::runtime(
                            "http.request: first argument must be a method string or an options table",
                        ));
                    }
                };
                // ── End argument parsing ──────────────────────────────────────

                // Store callback in Lua registry
                let callback_key = lua.create_registry_value(callback).map_err(|e| {
                    mlua::Error::external(format!("http.request: failed to store callback: {e}"))
                })?;

                // Generate request ID, check concurrency cap, register pending callback
                let request_id = {
                    let mut entries = registry.lock().expect("HttpAsyncEntries mutex poisoned");

                    if entries.in_flight >= MAX_CONCURRENT_HTTP_REQUESTS {
                        // Clean up the callback key we just stored
                        let _ = lua.remove_registry_value(callback_key);
                        return Ok((
                            None::<String>,
                            Some(format!(
                                "Too many concurrent HTTP requests (limit: {MAX_CONCURRENT_HTTP_REQUESTS})"
                            )),
                        ));
                    }

                    let id = format!("http_{}", entries.next_id);
                    entries.next_id += 1;
                    entries.in_flight += 1;
                    entries.pending.insert(id.clone(), callback_key);
                    id
                };

                // Extract all data needed by the background thread (plain Rust types only)
                let thread_method = method.to_uppercase();
                let thread_url = url;
                let thread_headers = opts.headers;
                let thread_json_body = opts.json_body;
                let thread_raw_body = opts.raw_body;
                let thread_timeout = opts.timeout;
                let thread_request_id = request_id.clone();
                let thread_registry = Arc::clone(&registry);
                // Clone the shared client — cheap (Arc internally) and shares the connection pool.
                let thread_client = registry.lock().expect("HttpAsyncEntries mutex poisoned").client.clone();

                // Spawn with Builder so we can handle spawn failure
                let spawn_result = std::thread::Builder::new()
                    .name(format!("http-{thread_method}-{}", &thread_request_id))
                    .spawn(move || {
                        let client = thread_client;

                        let mut builder = match thread_method.as_str() {
                            "GET" => client.get(&thread_url),
                            "POST" => client.post(&thread_url),
                            "PUT" => client.put(&thread_url),
                            "DELETE" => client.delete(&thread_url),
                            "PATCH" => client.patch(&thread_url),
                            "HEAD" => client.head(&thread_url),
                            other => {
                                let mut entries =
                                    thread_registry.lock().expect("HttpAsyncEntries mutex poisoned");
                                entries.emit_response(CompletedHttpResponse {
                                    request_id: thread_request_id,
                                    result: Err(format!("Unsupported HTTP method: {other}")),
                                });
                                entries.in_flight = entries.in_flight.saturating_sub(1);
                                return;
                            }
                        };

                        // Apply per-request timeout — overrides the shared client's default.
                        builder = builder.timeout(thread_timeout);

                        // Apply headers
                        for (k, v) in &thread_headers {
                            builder = builder.header(k.as_str(), v.as_str());
                        }

                        // Apply body
                        if let Some(ref json) = thread_json_body {
                            builder = builder.json(json);
                        } else if let Some(ref body) = thread_raw_body {
                            builder = builder.body(body.clone());
                        }

                        // Execute request
                        let result = match builder.send() {
                            Ok(resp) => {
                                let status = resp.status().as_u16();
                                let headers: Vec<(String, String)> = resp
                                    .headers()
                                    .iter()
                                    .filter_map(|(name, value)| {
                                        value
                                            .to_str()
                                            .ok()
                                            .map(|v| (name.as_str().to_string(), v.to_string()))
                                    })
                                    .collect();
                                let body = resp.text().unwrap_or_default();
                                Ok(HttpResponseData {
                                    status,
                                    body,
                                    headers,
                                })
                            }
                            Err(e) => Err(format!("HTTP {thread_method} failed: {e}")),
                        };

                        // Emit completed response and decrement in-flight counter
                        let mut entries =
                            thread_registry.lock().expect("HttpAsyncEntries mutex poisoned");
                        entries.emit_response(CompletedHttpResponse {
                            request_id: thread_request_id,
                            result,
                        });
                        entries.in_flight = entries.in_flight.saturating_sub(1);
                    });

                // Handle spawn failure: roll back in_flight and pending
                if let Err(e) = spawn_result {
                    let mut entries = registry.lock().expect("HttpAsyncEntries mutex poisoned");
                    entries.in_flight = entries.in_flight.saturating_sub(1);
                    if let Some(key) = entries.pending.remove(&request_id) {
                        let _ = lua.remove_registry_value(key);
                    }
                    return Ok((
                        None::<String>,
                        Some(format!("Failed to spawn HTTP thread: {e}")),
                    ));
                }

                Ok((Some(request_id), None::<String>))
            },
        )
        .map_err(|e| anyhow!("Failed to create http.request function: {e}"))?;

    http_table
        .set("request", request_fn)
        .map_err(|e| anyhow!("Failed to set http.request: {e}"))?;

    lua.globals()
        .set("http", http_table)
        .map_err(|e| anyhow!("Failed to register http table globally: {e}"))?;

    Ok(())
}

/// Poll for completed async HTTP responses and fire Lua callbacks.
///
/// Called from the Hub tick loop each tick. For each completed response:
/// - Remove the pending callback from the registry
/// - Build a response table (or error string)
/// - Fire the callback
/// - Clean up the Lua registry key
///
/// # Deadlock Prevention
///
/// Completed responses and callback keys are collected under the lock,
/// then the lock is released before calling Lua. This allows callbacks
/// to issue new `http.request()` calls without deadlocking.
///
/// # Returns
///
/// The number of HTTP callbacks fired.
pub fn poll_http_responses(lua: &Lua, registry: &HttpAsyncRegistry) -> usize {
    // Phase 1: drain responses and collect callback keys under the lock.
    let fired: Vec<(mlua::RegistryKey, std::result::Result<HttpResponseData, String>)> = {
        let mut entries = registry.lock().expect("HttpAsyncEntries mutex poisoned");

        if entries.responses.is_empty() {
            return 0;
        }

        let responses: Vec<CompletedHttpResponse> = entries.responses.drain(..).collect();
        let mut fired = Vec::with_capacity(responses.len());

        for response in responses {
            if let Some(callback_key) = entries.pending.remove(&response.request_id) {
                fired.push((callback_key, response.result));
            } else {
                log::warn!(
                    "[http] Response for unknown request_id: {}",
                    response.request_id
                );
            }
        }

        fired
    };
    // Lock released here — callbacks can safely call http.request().

    // Phase 2: fire callbacks without holding the lock.
    let count = fired.len();

    for (callback_key, result) in &fired {
        let callback_result: mlua::Result<()> = (|| {
            let callback: mlua::Function = lua.registry_value(callback_key)?;

            match result {
                Ok(data) => {
                    // Build response table
                    let table = lua.create_table()?;
                    table.set("status", data.status)?;
                    table.set("body", lua.create_string(&data.body)?)?;

                    let headers_table = lua.create_table()?;
                    for (k, v) in &data.headers {
                        headers_table.set(
                            lua.create_string(k.as_str())?,
                            lua.create_string(v.as_str())?,
                        )?;
                    }
                    table.set("headers", headers_table)?;

                    callback.call::<()>((table, mlua::Value::Nil))?;
                }
                Err(err_msg) => {
                    callback.call::<()>((mlua::Value::Nil, err_msg.as_str()))?;
                }
            }
            Ok(())
        })();

        if let Err(e) = callback_result {
            log::warn!("[http] Async callback error: {e}");
        }
    }

    // Phase 3: clean up callback registry keys.
    for (callback_key, _) in fired {
        let _ = lua.remove_registry_value(callback_key);
    }

    count
}

/// Fire the Lua callback for a single completed HTTP response.
///
/// Called from `handle_hub_event()` when an `HubEvent::HttpResponse` arrives
/// via the event channel. Looks up the pending callback by `request_id`,
/// fires it with the response data, and cleans up the registry key.
///
/// This is the event-driven counterpart of [`poll_http_responses`], which
/// batch-drains the shared vec. Both use the same callback-firing logic.
pub(crate) fn fire_single_http_callback(
    lua: &Lua,
    registry: &HttpAsyncRegistry,
    response: CompletedHttpResponse,
) {
    // Look up and remove the callback key under the lock.
    let callback_key = {
        let mut entries = registry.lock().expect("HttpAsyncEntries mutex poisoned");
        entries.pending.remove(&response.request_id)
    };

    let Some(callback_key) = callback_key else {
        log::warn!(
            "[http] Event response for unknown request_id: {}",
            response.request_id
        );
        return;
    };

    // Fire callback without holding the lock (allows re-entrant http.request).
    let callback_result: mlua::Result<()> = (|| {
        let callback: mlua::Function = lua.registry_value(&callback_key)?;

        match &response.result {
            Ok(data) => {
                let table = lua.create_table()?;
                table.set("status", data.status)?;
                table.set("body", lua.create_string(&data.body)?)?;

                let headers_table = lua.create_table()?;
                for (k, v) in &data.headers {
                    headers_table.set(
                        lua.create_string(k.as_str())?,
                        lua.create_string(v.as_str())?,
                    )?;
                }
                table.set("headers", headers_table)?;

                callback.call::<()>((table, mlua::Value::Nil))?;
            }
            Err(err_msg) => {
                callback.call::<()>((mlua::Value::Nil, err_msg.as_str()))?;
            }
        }
        Ok(())
    })();

    if let Err(e) = callback_result {
        log::warn!("[http] Async event callback error: {e}");
    }

    // Clean up the registry key.
    let _ = lua.remove_registry_value(callback_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Function;

    #[test]
    fn test_http_table_created() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let globals = lua.globals();
        let http_table: Table = globals.get("http").expect("http table should exist");

        let _: Function = http_table.get("get").expect("http.get should exist");
        let _: Function = http_table.get("post").expect("http.post should exist");
        let _: Function = http_table.get("put").expect("http.put should exist");
        let _: Function = http_table.get("delete").expect("http.delete should exist");
    }

    #[test]
    fn test_get_invalid_url_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let (resp, err): (Option<Table>, Option<String>) = lua
            .load(r#"return http.get("not-a-valid-url")"#)
            .eval()
            .expect("http.get should be callable");

        assert!(resp.is_none());
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("HTTP GET failed"),
            "Error should describe the failure"
        );
    }

    #[test]
    fn test_post_invalid_url_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let (resp, err): (Option<Table>, Option<String>) = lua
            .load(r#"return http.post("not-a-valid-url")"#)
            .eval()
            .expect("http.post should be callable");

        assert!(resp.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn test_put_invalid_url_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let (resp, err): (Option<Table>, Option<String>) = lua
            .load(r#"return http.put("not-a-valid-url")"#)
            .eval()
            .expect("http.put should be callable");

        assert!(resp.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn test_delete_invalid_url_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let (resp, err): (Option<Table>, Option<String>) = lua
            .load(r#"return http.delete("not-a-valid-url")"#)
            .eval()
            .expect("http.delete should be callable");

        assert!(resp.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn test_get_connection_refused_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        // Use a port that's almost certainly not listening
        let (resp, err): (Option<Table>, Option<String>) = lua
            .load(r#"return http.get("http://127.0.0.1:1", { timeout_ms = 1000 })"#)
            .eval()
            .expect("http.get should be callable");

        assert!(resp.is_none());
        assert!(err.is_some());
    }

    #[test]
    fn test_parse_opts_empty() {
        let lua = Lua::new();
        let opts = parse_opts(&lua, None).expect("Should parse empty opts");
        assert!(opts.headers.is_empty());
        assert!(opts.json_body.is_none());
        assert!(opts.raw_body.is_none());
        assert_eq!(opts.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn test_parse_opts_with_timeout() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("timeout_ms", 5000u64).unwrap();

        let opts = parse_opts(&lua, Some(table)).expect("Should parse opts with timeout");
        assert_eq!(opts.timeout, Duration::from_millis(5000));
    }

    #[test]
    fn test_parse_opts_with_headers() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        let headers = lua.create_table().unwrap();
        headers.set("Content-Type", "application/json").unwrap();
        headers.set("Authorization", "Bearer token").unwrap();
        table.set("headers", headers).unwrap();

        let opts = parse_opts(&lua, Some(table)).expect("Should parse opts with headers");
        assert_eq!(opts.headers.len(), 2);
    }

    #[test]
    fn test_parse_opts_with_body() {
        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        table.set("body", "raw content").unwrap();

        let opts = parse_opts(&lua, Some(table)).expect("Should parse opts with body");
        assert_eq!(opts.raw_body, Some("raw content".to_string()));
    }

    #[test]
    fn test_parse_opts_with_json() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("register");

        let table: Table = lua
            .load(r#"return { json = { name = "test" } }"#)
            .eval()
            .unwrap();

        let opts = parse_opts(&lua, Some(table)).expect("Should parse opts with json");
        assert!(opts.json_body.is_some());
        assert_eq!(opts.json_body.unwrap()["name"], "test");
    }

    #[test]
    fn test_http_request_function_exists() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let globals = lua.globals();
        let http_table: Table = globals.get("http").expect("http table should exist");
        let _: Function = http_table
            .get("request")
            .expect("http.request should exist");
    }

    #[test]
    fn test_http_request_returns_id() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        // Request to an invalid URL — will fail in the background thread
        let (id, err): (Option<String>, Option<String>) = lua
            .load(
                r#"return http.request("GET", "not-a-valid-url", {}, function(resp, err) end)"#,
            )
            .eval()
            .expect("http.request should be callable");

        assert!(err.is_none(), "Should not error: {:?}", err);
        let id = id.expect("Should return a request ID");
        assert!(id.starts_with("http_"), "ID should start with 'http_', got: {id}");

        // Pending should have 1 entry, in_flight should be 1
        let entries = registry.lock().unwrap();
        assert_eq!(entries.pending_count(), 1);
        assert_eq!(entries.in_flight_count(), 1);
    }

    #[test]
    fn test_http_request_async_error_callback() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        // Set up a global to capture the callback result
        lua.load(
            r#"
            _test_result = nil
            _test_err = nil
            http.request("GET", "not-a-valid-url", { timeout_ms = 1000 }, function(resp, err)
                _test_result = resp
                _test_err = err
            end)
            "#,
        )
        .exec()
        .expect("http.request should be callable");

        // Wait for the background thread to complete
        let max_wait = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();
        loop {
            let has_responses = {
                let entries = registry.lock().unwrap();
                !entries.responses.is_empty()
            };
            if has_responses {
                break;
            }
            if start.elapsed() > max_wait {
                panic!("Background HTTP thread did not complete in time");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Poll responses — should fire the callback
        let fired = poll_http_responses(&lua, &registry);
        assert_eq!(fired, 1, "Should have fired exactly 1 callback");

        // Check the callback received an error
        let err: Option<String> = lua
            .load(r#"return _test_err"#)
            .eval()
            .expect("Should read _test_err");
        assert!(err.is_some(), "Should have received an error");
        assert!(
            err.unwrap().contains("HTTP GET failed"),
            "Error should describe the failure"
        );

        // Response should be nil
        let resp: Value = lua
            .load(r#"return _test_result"#)
            .eval()
            .expect("Should read _test_result");
        assert_eq!(resp, Value::Nil, "Response should be nil on error");
    }

    #[test]
    fn test_poll_http_no_responses_returns_zero() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        // No requests made — poll should return 0 immediately
        let fired = poll_http_responses(&lua, &registry);
        assert_eq!(fired, 0);
    }

    #[test]
    fn test_http_request_concurrency_cap() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        // Artificially set in_flight to the max
        {
            let mut entries = registry.lock().unwrap();
            entries.in_flight = MAX_CONCURRENT_HTTP_REQUESTS;
        }

        // Next request should be rejected
        let (id, err): (Option<String>, Option<String>) = lua
            .load(
                r#"return http.request("GET", "http://example.com", {}, function() end)"#,
            )
            .eval()
            .expect("http.request should be callable");

        assert!(id.is_none(), "Should not return an ID when at capacity");
        assert!(err.is_some(), "Should return an error");
        assert!(
            err.unwrap().contains("Too many concurrent"),
            "Error should mention concurrency limit"
        );

        // Pending should still be 0 (the request was rejected)
        let entries = registry.lock().unwrap();
        assert_eq!(entries.pending_count(), 0);
    }

    // ── http.request() calling convention tests ───────────────────────────

    #[test]
    fn test_http_request_table_first_form_returns_id() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        let (id, err): (Option<String>, Option<String>) = lua
            .load(r#"return http.request(
                { method = "GET", url = "not-a-valid-url" },
                function(resp, err) end
            )"#)
            .eval()
            .expect("http.request table-first form should be callable");

        assert!(err.is_none(), "Should not error on valid call: {:?}", err);
        let id = id.expect("Should return a request ID");
        assert!(id.starts_with("http_"), "ID should start with 'http_', got: {id}");
    }

    #[test]
    fn test_http_request_table_first_with_opts_returns_id() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        let (id, err): (Option<String>, Option<String>) = lua
            .load(r#"return http.request({
                method     = "GET",
                url        = "not-a-valid-url",
                timeout_ms = 1000,
                headers    = { ["X-Test"] = "yes" },
            }, function(resp, err) end)"#)
            .eval()
            .expect("http.request table-first with opts should be callable");

        assert!(err.is_none(), "Should not error on valid call: {:?}", err);
        assert!(id.is_some(), "Should return a request ID");
    }

    #[test]
    fn test_http_request_three_arg_form_returns_id() {
        // Positional 3-arg form: http.request("METHOD", "url", callback)
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        let (id, err): (Option<String>, Option<String>) = lua
            .load(r#"return http.request("GET", "not-a-valid-url", function(resp, err) end)"#)
            .eval()
            .expect("http.request 3-arg form should be callable");

        assert!(err.is_none(), "Should not error on valid 3-arg call: {:?}", err);
        assert!(id.is_some(), "Should return a request ID");
    }

    #[test]
    fn test_http_request_table_first_missing_method_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let result: mlua::Result<(Option<String>, Option<String>)> = lua
            .load(r#"return http.request({ url = "https://example.com" }, function() end)"#)
            .eval();

        // Should return a Lua runtime error (not a (nil, err) tuple)
        assert!(result.is_err(), "Should error when 'method' field is missing");
    }

    #[test]
    fn test_http_request_table_first_missing_url_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let result: mlua::Result<(Option<String>, Option<String>)> = lua
            .load(r#"return http.request({ method = "GET" }, function() end)"#)
            .eval();

        assert!(result.is_err(), "Should error when 'url' field is missing");
    }

    #[test]
    fn test_http_request_wrong_first_arg_returns_error() {
        let lua = Lua::new();
        register(&lua, new_http_registry()).expect("Should register http primitives");

        let result: mlua::Result<(Option<String>, Option<String>)> = lua
            .load(r#"return http.request(42, "https://example.com", function() end)"#)
            .eval();

        assert!(result.is_err(), "Should error when first arg is not a string or table");
    }

    #[test]
    fn test_http_request_table_first_async_error_callback() {
        // End-to-end: table-first form fires the callback with an error for invalid URLs.
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        lua.load(r#"
            _test_result = nil
            _test_err    = nil
            http.request({ method = "GET", url = "not-a-valid-url", timeout_ms = 1000 }, function(resp, err)
                _test_result = resp
                _test_err    = err
            end)
        "#)
        .exec()
        .expect("http.request table-first should be callable");

        // Wait for background thread
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let done = { !registry.lock().unwrap().responses.is_empty() };
            if done { break; }
            assert!(std::time::Instant::now() < deadline, "Background HTTP thread timed out");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let fired = poll_http_responses(&lua, &registry);
        assert_eq!(fired, 1);

        let err: Option<String> = lua.load(r#"return _test_err"#).eval().unwrap();
        assert!(err.is_some(), "Callback should receive an error");
        assert!(err.unwrap().contains("HTTP GET failed"));
    }

    #[test]
    fn test_http_request_in_flight_decrements_after_completion() {
        let lua = Lua::new();
        let registry = new_http_registry();
        register(&lua, Arc::clone(&registry)).expect("Should register http primitives");

        // Fire a request to an invalid URL (will fail quickly)
        lua.load(
            r#"http.request("GET", "not-a-valid-url", { timeout_ms = 1000 }, function() end)"#,
        )
        .exec()
        .expect("http.request should be callable");

        // Wait for background thread
        let max_wait = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();
        loop {
            let has_responses = {
                let entries = registry.lock().unwrap();
                !entries.responses.is_empty()
            };
            if has_responses {
                break;
            }
            if start.elapsed() > max_wait {
                panic!("Background HTTP thread did not complete in time");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // After thread completes, in_flight should be 0
        let entries = registry.lock().unwrap();
        assert_eq!(entries.in_flight_count(), 0, "in_flight should decrement after thread completes");
    }
}
