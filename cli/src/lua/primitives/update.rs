//! Self-update primitive for Lua scripts.
//!
//! Exposes version checking and update installation to Lua, allowing
//! browser commands to trigger CLI updates remotely.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Check for updates (15s timeout)
//! local status = update.check()
//! -- status = { status = "available", current = "0.5.0", latest = "0.6.0" }
//!
//! -- Install update and exec-restart
//! local result = update.install()
//! -- On success, process restarts (this never returns)
//! -- On error: result = { error = "Failed to download..." }
//! ```

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use super::HubEventSender;
use crate::commands::update::{self, UpdateStatus};
use crate::hub::events::HubEvent;
use crate::lua::primitives::HubRequest;

/// Guard against concurrent install calls (e.g., two browsers racing).
static INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Register the `update` table with self-update primitives.
///
/// Creates a global `update` table with methods:
/// - `update.check()` — Check for available updates (15s timeout)
/// - `update.install()` — Download, verify, replace binary, then exec-restart
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    let update_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create update table: {e}"))?;

    // update.check() — Query GitHub for latest version (bounded timeout)
    let check_fn = lua
        .create_function(|lua, ()| {
            let status =
                update::get_update_status_with_timeout().map_err(LuaError::external)?;

            let table = lua.create_table()?;
            match status {
                UpdateStatus::UpdateAvailable { current, latest } => {
                    table.set("status", "available")?;
                    table.set("current", current)?;
                    table.set("latest", latest)?;
                }
                UpdateStatus::UpToDate { version } => {
                    table.set("status", "up_to_date")?;
                    table.set("current", version)?;
                }
                UpdateStatus::AheadOfRelease { current, latest } => {
                    table.set("status", "ahead")?;
                    table.set("current", current)?;
                    table.set("latest", latest)?;
                }
            }
            Ok(table)
        })
        .map_err(|e| anyhow!("Failed to create update.check function: {e}"))?;
    update_table
        .set("check", check_fn)
        .map_err(|e| anyhow!("Failed to set update.check: {e}"))?;

    // update.install() — Download, replace binary, request exec-restart
    let tx = hub_event_tx;
    let install_fn = lua
        .create_function(move |lua, ()| {
            // Guard against concurrent install attempts
            if INSTALL_IN_PROGRESS.swap(true, Ordering::SeqCst) {
                let table = lua.create_table()?;
                table.set("error", "Update already in progress")?;
                return Ok(table);
            }

            // install() downloads and replaces the binary synchronously
            let install_result = update::install();

            if let Err(e) = install_result {
                INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
                let table = lua.create_table()?;
                table.set("error", e.to_string())?;
                return Ok(table);
            }

            // Binary replaced — request exec-restart via hub event loop.
            // Don't reset INSTALL_IN_PROGRESS; the process is about to restart.
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::ExecRestart));
            } else {
                log::warn!("[Update] install() called before hub_event_tx set");
            }

            let table = lua.create_table()?;
            table.set("success", true)?;
            Ok(table)
        })
        .map_err(|e| anyhow!("Failed to create update.install function: {e}"))?;
    update_table
        .set("install", install_fn)
        .map_err(|e| anyhow!("Failed to set update.install: {e}"))?;

    lua.globals()
        .set("update", update_table)
        .map_err(|e| anyhow!("Failed to register update table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    use crate::lua::primitives::new_hub_event_sender;

    #[test]
    fn test_update_table_created() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx).expect("Should register update primitives");

        let globals = lua.globals();
        let update_table: Table = globals.get("update").expect("update table should exist");

        let _: Function = update_table.get("check").expect("update.check should exist");
        let _: Function = update_table
            .get("install")
            .expect("update.install should exist");
    }

    #[test]
    fn test_install_guard_prevents_concurrent() {
        // Reset the flag for this test
        INSTALL_IN_PROGRESS.store(true, Ordering::SeqCst);

        let lua = Lua::new();
        let tx = new_hub_event_sender();
        register(&lua, tx).expect("Should register");

        let result: Table = lua
            .load("return update.install()")
            .eval()
            .expect("Should return table");
        let error: String = result.get("error").expect("Should have error");
        assert_eq!(error, "Update already in progress");

        // Clean up
        INSTALL_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}
