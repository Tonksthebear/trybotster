//! Connection/pairing primitives for Lua scripts.
//!
//! Exposes connection URL and code regeneration to Lua, allowing scripts to
//! query the current pairing URL and request code regeneration.
//!
//! # Design Principle: "Query freely. Mutate via queue."
//!
//! - **Queries** (`get_url`) read directly from `HandleCache` - non-blocking,
//!   thread-safe snapshot
//! - **Mutations** (`regenerate`) queue requests for Hub to process
//!   asynchronously after Lua callbacks return
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Get the current connection URL (non-blocking, reads from cache)
//! local url, err = connection.get_url()
//! if url then
//!     log.info("Connection URL: " .. url)
//! else
//!     log.warn("No connection URL: " .. (err or "unknown"))
//! end
//!
//! -- Request code regeneration (async - Hub processes after callback)
//! connection.regenerate()
//! ```

// Rust guideline compliant 2026-02-03

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::hub::handle_cache::HandleCache;

/// Connection operation requests queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug, Clone)]
pub enum ConnectionRequest {
    /// Generate the connection URL (lazy, with auto-regeneration).
    ///
    /// Hub calls `generate_connection_url()` which:
    /// - Generates the PreKeyBundle on first call
    /// - Auto-regenerates if the previous bundle was consumed by a browser
    /// - Caches the result in HandleCache
    /// - Fires `connection_code_ready` Lua event for broadcast
    Generate,

    /// Force-regenerate the connection code (PreKeyBundle).
    ///
    /// Hub will regenerate the Signal Protocol bundle unconditionally
    /// and update the cached connection URL.
    Regenerate,

    /// Copy the connection URL to the system clipboard.
    ///
    /// Hub generates the URL (fresh from current Signal bundle) and
    /// copies it to clipboard via `arboard::Clipboard`.
    CopyToClipboard,
}

/// Shared request queue for connection operations from Lua.
pub type ConnectionRequestQueue = Arc<Mutex<Vec<ConnectionRequest>>>;

/// Create a new connection request queue.
#[must_use]
pub fn new_request_queue() -> ConnectionRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Register connection primitives with the Lua state.
///
/// Adds the following functions to the `connection` table:
/// - `connection.get_url()` - Get the cached connection URL (or nil + error)
/// - `connection.regenerate()` - Request connection code regeneration (async)
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for connection operations (processed by Hub)
/// * `handle_cache` - Thread-safe cache for connection URL queries
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(
    lua: &Lua,
    request_queue: ConnectionRequestQueue,
    handle_cache: Arc<HandleCache>,
) -> Result<()> {
    let connection = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create connection table: {e}"))?;

    // connection.get_url() -> url_string_or_nil, error_string_or_nil
    //
    // Returns two values following Lua convention:
    //   success: url, nil
    //   failure: nil, error_message
    let cache = Arc::clone(&handle_cache);
    let get_url_fn = lua
        .create_function(move |_, ()| {
            match cache.get_connection_url() {
                Ok(url) => Ok((Some(url), None::<String>)),
                Err(err) => Ok((None::<String>, Some(err))),
            }
        })
        .map_err(|e| anyhow!("Failed to create connection.get_url function: {e}"))?;

    connection
        .set("get_url", get_url_fn)
        .map_err(|e| anyhow!("Failed to set connection.get_url: {e}"))?;

    // connection.generate() - Queue a lazy generation request
    //
    // Triggers Hub-side generation which:
    // - Creates the PreKeyBundle on first call
    // - Auto-regenerates if previous bundle was consumed
    // - Caches result in HandleCache
    // - Fires connection_code_ready Lua event for broadcast
    let generate_queue = Arc::clone(&request_queue);
    let generate_fn = lua
        .create_function(move |_, ()| {
            let mut q = generate_queue
                .lock()
                .expect("Connection request queue mutex poisoned");
            q.push(ConnectionRequest::Generate);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create connection.generate function: {e}"))?;

    connection
        .set("generate", generate_fn)
        .map_err(|e| anyhow!("Failed to set connection.generate: {e}"))?;

    // connection.regenerate() - Queue a forced code regeneration request
    let regen_queue = Arc::clone(&request_queue);
    let regenerate_fn = lua
        .create_function(move |_, ()| {
            let mut q = regen_queue
                .lock()
                .expect("Connection request queue mutex poisoned");
            q.push(ConnectionRequest::Regenerate);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create connection.regenerate function: {e}"))?;

    connection
        .set("regenerate", regenerate_fn)
        .map_err(|e| anyhow!("Failed to set connection.regenerate: {e}"))?;

    // connection.copy_to_clipboard() - Copy connection URL to system clipboard
    let clipboard_queue = request_queue;
    let copy_fn = lua
        .create_function(move |_, ()| {
            let mut q = clipboard_queue
                .lock()
                .expect("Connection request queue mutex poisoned");
            q.push(ConnectionRequest::CopyToClipboard);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create connection.copy_to_clipboard function: {e}"))?;

    connection
        .set("copy_to_clipboard", copy_fn)
        .map_err(|e| anyhow!("Failed to set connection.copy_to_clipboard: {e}"))?;

    // Register globally
    lua.globals()
        .set("connection", connection)
        .map_err(|e| anyhow!("Failed to register connection table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_queue_and_cache() -> (ConnectionRequestQueue, Arc<HandleCache>) {
        (new_request_queue(), Arc::new(HandleCache::new()))
    }

    #[test]
    fn test_register_creates_connection_table() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register connection primitives");

        let conn: LuaTable = lua
            .globals()
            .get("connection")
            .expect("connection table should exist");
        assert!(conn.contains_key("get_url").unwrap());
        assert!(conn.contains_key("generate").unwrap());
        assert!(conn.contains_key("regenerate").unwrap());
        assert!(conn.contains_key("copy_to_clipboard").unwrap());
    }

    #[test]
    fn test_get_url_returns_nil_and_error_when_not_generated() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, queue, cache).expect("Should register");

        let (url, err): (Option<String>, Option<String>) = lua
            .load("return connection.get_url()")
            .eval()
            .unwrap();

        assert!(url.is_none(), "URL should be nil when not generated");
        assert!(err.is_some(), "Error should be present");
        assert!(
            err.unwrap().contains("not yet generated"),
            "Error should mention not yet generated"
        );
    }

    #[test]
    fn test_get_url_returns_cached_url() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        // Pre-populate the cache
        cache.set_connection_url(Ok("https://example.com/connect#abc123".to_string()));

        register(&lua, queue, cache).expect("Should register");

        let (url, err): (Option<String>, Option<String>) = lua
            .load("return connection.get_url()")
            .eval()
            .unwrap();

        assert_eq!(url, Some("https://example.com/connect#abc123".to_string()));
        assert!(err.is_none(), "Error should be nil on success");
    }

    #[test]
    fn test_get_url_returns_cached_error() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        // Pre-populate with an error
        cache.set_connection_url(Err("Signal protocol init failed".to_string()));

        register(&lua, queue, cache).expect("Should register");

        let (url, err): (Option<String>, Option<String>) = lua
            .load("return connection.get_url()")
            .eval()
            .unwrap();

        assert!(url.is_none());
        assert_eq!(err, Some("Signal protocol init failed".to_string()));
    }

    #[test]
    fn test_generate_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load("connection.generate()").exec().unwrap();

        let requests = queue
            .lock()
            .expect("Connection request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], ConnectionRequest::Generate));
    }

    #[test]
    fn test_regenerate_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load("connection.regenerate()").exec().unwrap();

        let requests = queue
            .lock()
            .expect("Connection request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], ConnectionRequest::Regenerate));
    }

    #[test]
    fn test_multiple_regenerate_requests_queue_in_order() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load(
            r#"
            connection.regenerate()
            connection.regenerate()
            connection.regenerate()
        "#,
        )
        .exec()
        .unwrap();

        let requests = queue
            .lock()
            .expect("Connection request queue mutex poisoned");
        assert_eq!(requests.len(), 3);
    }

    #[test]
    fn test_copy_to_clipboard_queues_request() {
        let lua = Lua::new();
        let (queue, cache) = create_test_queue_and_cache();

        register(&lua, Arc::clone(&queue), cache).expect("Should register");

        lua.load("connection.copy_to_clipboard()").exec().unwrap();

        let requests = queue
            .lock()
            .expect("Connection request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], ConnectionRequest::CopyToClipboard));
    }
}
