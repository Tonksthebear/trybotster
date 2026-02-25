//! Hub discovery primitives for Lua scripts.
//!
//! Exposes synchronous functions to discover running hubs on this machine,
//! check if a specific hub is running, and resolve socket paths.
//!
//! # Design
//!
//! All operations are synchronous reads — no event sender needed.
//! Functions delegate to `crate::hub::daemon` which reads PID files
//! and checks process liveness.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Discover all running hubs
//! local hubs = hub_discovery.list()
//! for _, h in ipairs(hubs) do
//!     log.info(h.id .. " pid=" .. h.pid .. " socket=" .. h.socket)
//! end
//!
//! -- Check if a specific hub is running
//! if hub_discovery.is_running("abc123") then
//!     log.info("Hub is alive")
//! end
//!
//! -- Get the socket path for a hub
//! local path = hub_discovery.socket_path("abc123")
//! ```
// Rust guideline compliant 2026-02

use anyhow::{anyhow, Result};
use mlua::Lua;

/// Register the `hub_discovery` table with discovery functions.
///
/// Creates a global `hub_discovery` table with methods:
/// - `hub_discovery.list()` — Discover all running hubs on this machine
/// - `hub_discovery.is_running(hub_id)` — Check if a specific hub is running
/// - `hub_discovery.socket_path(hub_id)` — Get the socket path for a hub
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(lua: &Lua) -> Result<()> {
    let table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create hub_discovery table: {e}"))?;

    // hub_discovery.list() -> table of { id, pid, socket }
    //
    // Returns an array of tables, one per running hub on this machine.
    // Each entry has: id (string), pid (number), socket (string).
    let list_fn = lua
        .create_function(|lua, ()| {
            let hubs = crate::hub::daemon::discover_running_hubs();
            let result = lua.create_table()?;

            for (i, (hub_id, pid)) in hubs.iter().enumerate() {
                let entry = lua.create_table()?;
                entry.set("id", hub_id.as_str())?;
                entry.set("pid", *pid)?;

                let socket = crate::hub::daemon::socket_path(hub_id)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                entry.set("socket", socket)?;

                result.set(i + 1, entry)?;
            }

            Ok(result)
        })
        .map_err(|e| anyhow!("Failed to create hub_discovery.list function: {e}"))?;

    table
        .set("list", list_fn)
        .map_err(|e| anyhow!("Failed to set hub_discovery.list: {e}"))?;

    // hub_discovery.is_running(hub_id) -> boolean
    //
    // Returns true if the hub with the given ID has a live process.
    let is_running_fn = lua
        .create_function(|_, hub_id: String| {
            Ok(crate::hub::daemon::is_hub_running(&hub_id))
        })
        .map_err(|e| anyhow!("Failed to create hub_discovery.is_running function: {e}"))?;

    table
        .set("is_running", is_running_fn)
        .map_err(|e| anyhow!("Failed to set hub_discovery.is_running: {e}"))?;

    // hub_discovery.socket_path(hub_id) -> string
    //
    // Returns the Unix socket path for the given hub ID.
    // The path is returned whether or not the hub is actually running.
    let socket_path_fn = lua
        .create_function(|_, hub_id: String| {
            let path = crate::hub::daemon::socket_path(&hub_id)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| mlua::Error::external(format!("Failed to get socket path: {e}")))?;
            Ok(path)
        })
        .map_err(|e| anyhow!("Failed to create hub_discovery.socket_path function: {e}"))?;

    table
        .set("socket_path", socket_path_fn)
        .map_err(|e| anyhow!("Failed to set hub_discovery.socket_path: {e}"))?;

    lua.globals()
        .set("hub_discovery", table)
        .map_err(|e| anyhow!("Failed to register hub_discovery table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    #[test]
    fn test_hub_discovery_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register hub_discovery primitives");

        let globals = lua.globals();
        let table: Table = globals
            .get("hub_discovery")
            .expect("hub_discovery table should exist");

        let _: Function = table.get("list").expect("hub_discovery.list should exist");
        let _: Function = table
            .get("is_running")
            .expect("hub_discovery.is_running should exist");
        let _: Function = table
            .get("socket_path")
            .expect("hub_discovery.socket_path should exist");
    }

    #[test]
    fn test_list_returns_table() {
        let lua = Lua::new();
        register(&lua).expect("Should register hub_discovery primitives");

        lua.load(
            r#"
            local hubs = hub_discovery.list()
            assert(type(hubs) == "table", "list() should return a table, got: " .. type(hubs))
        "#,
        )
        .exec()
        .expect("hub_discovery.list test should pass");
    }

    #[test]
    fn test_is_running_nonexistent_returns_false() {
        let lua = Lua::new();
        register(&lua).expect("Should register hub_discovery primitives");

        let result: bool = lua
            .load(r#"return hub_discovery.is_running("nonexistent_hub_xyz_12345")"#)
            .eval()
            .expect("hub_discovery.is_running should be callable");

        assert!(!result, "nonexistent hub should not be running");
    }

    #[test]
    fn test_socket_path_returns_expected_pattern() {
        let lua = Lua::new();
        register(&lua).expect("Should register hub_discovery primitives");

        let result: String = lua
            .load(r#"return hub_discovery.socket_path("test_hub")"#)
            .eval()
            .expect("hub_discovery.socket_path should be callable");

        assert!(
            result.contains("botster-"),
            "socket path should contain 'botster-', got: {result}"
        );
        assert!(
            result.ends_with("test_hub.sock"),
            "socket path should end with 'test_hub.sock', got: {result}"
        );
    }
}
