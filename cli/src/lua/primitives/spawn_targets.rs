//! Spawn target primitives for Lua scripts.
//!
//! Exposes the device-scoped spawn target registry and live inspection API to
//! Lua so future TUI and browser flows can admit and inspect targets without
//! owning authorization logic.

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::spawn_targets::SpawnTargetRegistry;

fn register_with_registry(lua: &Lua, registry: SpawnTargetRegistry) -> Result<()> {
    let spawn_targets = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create spawn_targets table: {e}"))?;

    let list_registry = registry.clone();
    let list_fn = lua
        .create_function(move |lua, ()| {
            let targets = list_registry
                .list()
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.list: {e}")))?;
            let value = serde_json::to_value(targets)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.list: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.list function: {e}"))?;
    spawn_targets
        .set("list", list_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.list: {e}"))?;

    let get_registry = registry.clone();
    let get_fn = lua
        .create_function(move |lua, id: String| {
            let target = get_registry
                .get(&id)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.get: {e}")))?;
            let value = serde_json::to_value(target)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.get: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.get function: {e}"))?;
    spawn_targets
        .set("get", get_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.get: {e}"))?;

    let add_registry = registry.clone();
    let add_fn = lua
        .create_function(
            move |lua, (path, name, plugins): (String, Option<String>, Option<Vec<String>>)| {
                let target = add_registry
                    .add(&path, name.as_deref(), plugins)
                    .map_err(|e| mlua::Error::runtime(format!("spawn_targets.add: {e}")))?;
                let value = serde_json::to_value(target)
                    .map_err(|e| mlua::Error::runtime(format!("spawn_targets.add: {e}")))?;
                super::json::json_to_lua(lua, &value)
            },
        )
        .map_err(|e| anyhow!("Failed to create spawn_targets.add function: {e}"))?;
    spawn_targets
        .set("add", add_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.add: {e}"))?;

    let update_registry = registry.clone();
    let update_fn = lua
        .create_function(
            move |lua,
                  (id, name, enabled, plugins): (
                String,
                Option<String>,
                Option<bool>,
                Option<Vec<String>>,
            )| {
                let target = update_registry
                    .update(&id, name.as_deref(), enabled, plugins)
                    .map_err(|e| mlua::Error::runtime(format!("spawn_targets.update: {e}")))?;
                let value = serde_json::to_value(target)
                    .map_err(|e| mlua::Error::runtime(format!("spawn_targets.update: {e}")))?;
                super::json::json_to_lua(lua, &value)
            },
        )
        .map_err(|e| anyhow!("Failed to create spawn_targets.update function: {e}"))?;
    spawn_targets
        .set("update", update_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.update: {e}"))?;

    let disable_registry = registry.clone();
    let disable_fn = lua
        .create_function(move |lua, id: String| {
            let target = disable_registry
                .disable(&id)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.disable: {e}")))?;
            let value = serde_json::to_value(target)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.disable: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.disable function: {e}"))?;
    spawn_targets
        .set("disable", disable_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.disable: {e}"))?;

    let enable_registry = registry.clone();
    let enable_fn = lua
        .create_function(move |lua, id: String| {
            let target = enable_registry
                .enable(&id)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.enable: {e}")))?;
            let value = serde_json::to_value(target)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.enable: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.enable function: {e}"))?;
    spawn_targets
        .set("enable", enable_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.enable: {e}"))?;

    let remove_registry = registry.clone();
    let remove_fn = lua
        .create_function(move |lua, id: String| {
            let target = remove_registry
                .remove(&id)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.remove: {e}")))?;
            let value = serde_json::to_value(target)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.remove: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.remove function: {e}"))?;
    spawn_targets
        .set("remove", remove_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.remove: {e}"))?;

    let inspect_registry = registry;
    let inspect_fn = lua
        .create_function(move |lua, path: String| {
            let inspection = inspect_registry
                .inspect(&path)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.inspect: {e}")))?;
            let value = serde_json::to_value(inspection)
                .map_err(|e| mlua::Error::runtime(format!("spawn_targets.inspect: {e}")))?;
            super::json::json_to_lua(lua, &value)
        })
        .map_err(|e| anyhow!("Failed to create spawn_targets.inspect function: {e}"))?;
    spawn_targets
        .set("inspect", inspect_fn)
        .map_err(|e| anyhow!("Failed to set spawn_targets.inspect: {e}"))?;

    lua.globals()
        .set("spawn_targets", spawn_targets)
        .map_err(|e| anyhow!("Failed to register spawn_targets table globally: {e}"))?;

    Ok(())
}

/// Register spawn target primitives with the Lua state.
///
/// Adds the following functions to the `spawn_targets` table:
/// - `spawn_targets.list()` - List all admitted targets
/// - `spawn_targets.get(target_id)` - Get one admitted target
/// - `spawn_targets.add(path, name?, plugins?)` - Admit a directory as a target
/// - `spawn_targets.update(target_id, name?, enabled?, plugins?)` - Update target fields
/// - `spawn_targets.disable(target_id)` - Disable a target
/// - `spawn_targets.enable(target_id)` - Enable a target
/// - `spawn_targets.remove(target_id)` - Remove a target
/// - `spawn_targets.inspect(path)` - Inspect a candidate path
pub(crate) fn register(lua: &Lua) -> Result<()> {
    register_with_registry(lua, SpawnTargetRegistry::load_default()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn register_creates_spawn_target_functions() {
        let temp = TempDir::new().unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let table: LuaTable = lua.globals().get("spawn_targets").unwrap();
        assert!(table.contains_key("list").unwrap());
        assert!(table.contains_key("get").unwrap());
        assert!(table.contains_key("add").unwrap());
        assert!(table.contains_key("update").unwrap());
        assert!(table.contains_key("disable").unwrap());
        assert!(table.contains_key("enable").unwrap());
        assert!(table.contains_key("remove").unwrap());
        assert!(table.contains_key("inspect").unwrap());
    }

    #[test]
    fn add_get_and_list_round_trip_through_lua() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        let id: String = lua
            .load(format!(
                r#"
                local target = spawn_targets.add("{path}", "Project")
                return target.id
            "#
            ))
            .eval()
            .unwrap();

        let listed: LuaTable = lua.load("return spawn_targets.list()").eval().unwrap();
        assert_eq!(listed.len().unwrap(), 1);

        let fetched: LuaTable = lua
            .load(format!(r#"return spawn_targets.get("{id}")"#))
            .eval()
            .unwrap();
        let name: String = fetched.get("name").unwrap();
        assert_eq!(name, "Project");
    }

    #[test]
    fn inspect_returns_live_admission_state() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        lua.load(format!(r#"spawn_targets.add("{path}", "Project")"#))
            .exec()
            .unwrap();

        let inspection: LuaTable = lua
            .load(format!(r#"return spawn_targets.inspect("{path}")"#))
            .eval()
            .unwrap();
        let admitted: bool = inspection.get("admitted").unwrap();
        let exists: bool = inspection.get("exists").unwrap();

        assert!(admitted);
        assert!(exists);
    }

    #[test]
    fn update_disable_enable_and_remove_round_trip_through_lua() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        let enabled: bool = lua
            .load(format!(
                r#"
                local target = spawn_targets.add("{path}", "Project")
                spawn_targets.disable(target.id)
                local renamed = spawn_targets.update(target.id, "Renamed", true)
                return renamed.enabled
            "#
            ))
            .eval()
            .unwrap();

        let removed: LuaValue = lua
            .load(format!(
                r#"
                local target = spawn_targets.list()[1]
                return spawn_targets.remove(target.id)
            "#
            ))
            .eval()
            .unwrap();

        assert!(enabled);
        assert!(!matches!(removed, LuaValue::Nil));
    }

    #[test]
    fn add_with_plugins_round_trips_through_lua() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        let result: LuaTable = lua
            .load(format!(
                r#"
                local target = spawn_targets.add("{path}", "Project", {{"github", "orchestrator"}})
                return target
            "#
            ))
            .eval()
            .unwrap();

        let plugins: LuaTable = result.get("plugins").unwrap();
        let p1: String = plugins.get(1).unwrap();
        let p2: String = plugins.get(2).unwrap();
        assert_eq!(p1, "github");
        assert_eq!(p2, "orchestrator");
    }

    #[test]
    fn add_without_plugins_returns_nil_in_lua() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        let is_nil: bool = lua
            .load(format!(
                r#"
                local target = spawn_targets.add("{path}", "Project")
                return target.plugins == nil
            "#
            ))
            .eval()
            .unwrap();
        assert!(is_nil);
    }

    #[test]
    fn update_plugins_through_lua() {
        let temp = TempDir::new().unwrap();
        let target_dir = temp.path().join("project");
        fs::create_dir_all(&target_dir).unwrap();
        let registry = SpawnTargetRegistry::new(temp.path().join("spawn_targets.json"));

        let lua = Lua::new();
        register_with_registry(&lua, registry).unwrap();

        let path = target_dir.to_string_lossy();
        let plugin: String = lua
            .load(format!(
                r#"
                local target = spawn_targets.add("{path}", "Project")
                local updated = spawn_targets.update(target.id, nil, nil, {{"messaging"}})
                return updated.plugins[1]
            "#
            ))
            .eval()
            .unwrap();
        assert_eq!(plugin, "messaging");
    }
}
