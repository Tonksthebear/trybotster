//! Logging primitive for Lua scripts.
//!
//! Exposes Rust's `log` crate to Lua scripts via a `log` table with
//! methods for each log level.
//!
//! # Usage in Lua
//!
//! ```lua
//! log.info("Application started")
//! log.warn("Configuration not found, using defaults")
//! log.error("Failed to connect to server")
//! log.debug("Processing item: " .. item_id)
//! ```
//!
//! Messages are routed through Rust's `log` crate, so they appear in
//! the same output as Rust log messages and respect the configured
//! log level filters.

use anyhow::{anyhow, Result};
use mlua::Lua;

/// Register the `log` table with logging functions.
///
/// Creates a global `log` table with methods:
/// - `log.info(msg)` - Info level message
/// - `log.warn(msg)` - Warning level message
/// - `log.error(msg)` - Error level message
/// - `log.debug(msg)` - Debug level message
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua) -> Result<()> {
    let log_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create log table: {e}"))?;

    // log.info(msg)
    let info_fn = lua
        .create_function(|_, msg: String| {
            log::info!(target: "lua", "{}", msg);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create log.info function: {e}"))?;
    log_table
        .set("info", info_fn)
        .map_err(|e| anyhow!("Failed to set log.info: {e}"))?;

    // log.warn(msg)
    let warn_fn = lua
        .create_function(|_, msg: String| {
            log::warn!(target: "lua", "{}", msg);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create log.warn function: {e}"))?;
    log_table
        .set("warn", warn_fn)
        .map_err(|e| anyhow!("Failed to set log.warn: {e}"))?;

    // log.error(msg)
    let error_fn = lua
        .create_function(|_, msg: String| {
            log::error!(target: "lua", "{}", msg);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create log.error function: {e}"))?;
    log_table
        .set("error", error_fn)
        .map_err(|e| anyhow!("Failed to set log.error: {e}"))?;

    // log.debug(msg)
    let debug_fn = lua
        .create_function(|_, msg: String| {
            log::debug!(target: "lua", "{}", msg);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create log.debug function: {e}"))?;
    log_table
        .set("debug", debug_fn)
        .map_err(|e| anyhow!("Failed to set log.debug: {e}"))?;

    // Register the table globally
    lua.globals()
        .set("log", log_table)
        .map_err(|e| anyhow!("Failed to register log table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    #[test]
    fn test_log_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register log primitives");

        let globals = lua.globals();
        let log_table: Table = globals.get("log").expect("log table should exist");

        // Verify all functions exist
        let _: Function = log_table.get("info").expect("log.info should exist");
        let _: Function = log_table.get("warn").expect("log.warn should exist");
        let _: Function = log_table.get("error").expect("log.error should exist");
        let _: Function = log_table.get("debug").expect("log.debug should exist");
    }

    #[test]
    fn test_log_functions_callable() {
        let lua = Lua::new();
        register(&lua).expect("Should register log primitives");

        // These should not panic
        lua.load(r#"log.info("test info")"#)
            .exec()
            .expect("log.info should be callable");
        lua.load(r#"log.warn("test warn")"#)
            .exec()
            .expect("log.warn should be callable");
        lua.load(r#"log.error("test error")"#)
            .exec()
            .expect("log.error should be callable");
        lua.load(r#"log.debug("test debug")"#)
            .exec()
            .expect("log.debug should be callable");
    }
}
