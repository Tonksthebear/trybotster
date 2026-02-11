//! HTTP client primitives for Lua scripts.
//!
//! Provides both synchronous and asynchronous HTTP request functions.
//!
//! # Synchronous API
//!
//! `http.get/post/put/delete` block until the response arrives. Suitable
//! for init-time scripts that run before the tick loop starts.
//!
//! **Warning:** Sync calls freeze the entire Hub tick loop (WebRTC, PTY,
//! timers) for the duration of the request. Use `http.request()` for
//! runtime HTTP calls.
//!
//! # Asynchronous API
//!
//! `http.request(method, url, opts, callback)` spawns a background thread
//! and returns immediately. The callback fires on the next tick after the
//! response arrives. This is the recommended API for production use.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Sync: simple GET (init-time only)
//! local resp, err = http.get("https://api.example.com/data")
//! if resp then
//!     log.info("Status: " .. resp.status)
//!     log.info("Body: " .. resp.body)
//! end
//!
//! -- Async: non-blocking request (recommended)
//! http.request("GET", "https://api.example.com/data", {}, function(resp, err)
//!     if resp then
//!         log.info("Status: " .. resp.status)
//!     else
//!         log.error("Request failed: " .. err)
//!     end
//! end)
//!
//! -- Async: POST with JSON body
//! http.request("POST", "https://api.example.com/data", {
//!     json = { name = "bot", version = 1 },
//!     headers = { ["Authorization"] = "Bearer token" },
//! }, function(resp, err)
//!     if resp then log.info("Created!") end
//! end)
//! ```
//!
//! # Error Handling
//!
//! Functions that can fail return two values following Lua convention:
//! - Success: `value, nil`
//! - Failure: `nil, error_message`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Table, Value};

/// Default request timeout in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Maximum number of concurrent async HTTP requests.
/// Prevents thread exhaustion from rapid-fire `http.request()` calls.
const MAX_CONCURRENT_HTTP_REQUESTS: usize = 16;

// =============================================================================
// Async HTTP types
// =============================================================================

/// Completed HTTP response data (plain Rust types, no Lua references).
struct CompletedHttpResponse {
    request_id: String,
    result: std::result::Result<HttpResponseData, String>,
}

/// Successful HTTP response payload.
struct HttpResponseData {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}

/// Async HTTP registry tracking in-flight requests and completed responses.
///
/// Pending callbacks are stored as `LuaRegistryKey` (main-thread only).
/// Background threads push `CompletedHttpResponse` to the responses queue.
/// The tick loop drains responses and fires callbacks.
pub struct HttpAsyncEntries {
    /// Callbacks awaiting responses, keyed by request_id.
    pending: HashMap<String, mlua::RegistryKey>,
    /// Completed responses waiting to fire callbacks.
    responses: Vec<CompletedHttpResponse>,
    /// Counter for generating unique request IDs.
    next_id: u64,
    /// Number of background threads currently executing HTTP requests.
    in_flight: usize,
}

impl Default for HttpAsyncEntries {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            responses: Vec::new(),
            next_id: 0,
            in_flight: 0,
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

/// Register the `http` table with synchronous and asynchronous HTTP functions.
///
/// Creates a global `http` table with methods:
/// - `http.get(url, opts?)` - Sync GET (blocks tick loop)
/// - `http.post(url, opts?)` - Sync POST (blocks tick loop)
/// - `http.put(url, opts?)` - Sync PUT (blocks tick loop)
/// - `http.delete(url, opts?)` - Sync DELETE (blocks tick loop)
/// - `http.request(method, url, opts, callback)` - Async request (non-blocking)
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
            let client = reqwest::blocking::Client::builder()
                .timeout(opts.timeout)
                .build()
                .map_err(|e| mlua::Error::external(format!("Failed to create HTTP client: {e}")))?;

            let builder = apply_opts(client.get(&url), &opts);

            match builder.send() {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(format!("HTTP GET failed: {e}")))),
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
            let client = reqwest::blocking::Client::builder()
                .timeout(opts.timeout)
                .build()
                .map_err(|e| mlua::Error::external(format!("Failed to create HTTP client: {e}")))?;

            let builder = apply_opts(client.post(&url), &opts);

            match builder.send() {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(format!("HTTP POST failed: {e}")))),
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
            let client = reqwest::blocking::Client::builder()
                .timeout(opts.timeout)
                .build()
                .map_err(|e| mlua::Error::external(format!("Failed to create HTTP client: {e}")))?;

            let builder = apply_opts(client.put(&url), &opts);

            match builder.send() {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(format!("HTTP PUT failed: {e}")))),
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
            let client = reqwest::blocking::Client::builder()
                .timeout(opts.timeout)
                .build()
                .map_err(|e| mlua::Error::external(format!("Failed to create HTTP client: {e}")))?;

            let builder = apply_opts(client.delete(&url), &opts);

            match builder.send() {
                Ok(resp) => {
                    let table = build_response_table(lua, resp)?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<Table>, Some(format!("HTTP DELETE failed: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create http.delete function: {e}"))?;

    http_table
        .set("delete", delete_fn)
        .map_err(|e| anyhow!("Failed to set http.delete: {e}"))?;

    // http.request(method, url, opts, callback) -> (request_id, nil) or (nil, error)
    //
    // Async HTTP request. Spawns a background thread, returns immediately.
    // The callback fires on the next tick after the response arrives:
    //   callback(response_table, nil)  -- on success
    //   callback(nil, error_string)    -- on failure
    //
    // Returns (nil, error_string) if the concurrency limit is reached.
    let request_fn = lua
        .create_function(
            move |lua, (method, url, opts, callback): (String, String, Option<Table>, mlua::Function)| {
                let opts = parse_opts(lua, opts)?;

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

                // Spawn with Builder so we can handle spawn failure
                let spawn_result = std::thread::Builder::new()
                    .name(format!("http-{thread_method}-{}", &thread_request_id))
                    .spawn(move || {
                        // Build client inside the thread (avoids tokio-in-tokio panic)
                        let client = match reqwest::blocking::Client::builder()
                            .timeout(thread_timeout)
                            .build()
                        {
                            Ok(c) => c,
                            Err(e) => {
                                let mut entries =
                                    thread_registry.lock().expect("HttpAsyncEntries mutex poisoned");
                                entries.responses.push(CompletedHttpResponse {
                                    request_id: thread_request_id,
                                    result: Err(format!("Failed to create HTTP client: {e}")),
                                });
                                entries.in_flight = entries.in_flight.saturating_sub(1);
                                return;
                            }
                        };

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
                                entries.responses.push(CompletedHttpResponse {
                                    request_id: thread_request_id,
                                    result: Err(format!("Unsupported HTTP method: {other}")),
                                });
                                entries.in_flight = entries.in_flight.saturating_sub(1);
                                return;
                            }
                        };

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

                        // Push completed response and decrement in-flight counter
                        let mut entries =
                            thread_registry.lock().expect("HttpAsyncEntries mutex poisoned");
                        entries.responses.push(CompletedHttpResponse {
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

    #[test]
    fn test_http_in_flight_decrements_after_completion() {
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
