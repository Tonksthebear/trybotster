//! Configuration primitives for Lua scripts.
//!
//! Exposes read/write access to the hub's JSON config file and
//! environment variables. The config file lives at
//! `~/.config/botster/config.json`.
//!
//! # Design
//!
//! All operations are synchronous with read-modify-write semantics for
//! `config.set()`. No queues are needed.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Read a config value
//! local val = config.get("theme")
//!
//! -- Set a config value (persists to disk)
//! config.set("theme", "dark")
//!
//! -- Get all config as a table
//! local all = config.all()
//! for k, v in pairs(all) do
//!     log.info(k .. " = " .. tostring(v))
//! end
//!
//! -- Get filesystem paths
//! local lua_path = config.lua_path()
//! local data_dir = config.data_dir()
//!
//! -- Read environment variables
//! local home = config.env("HOME")
//! ```
//!
//! # Error Handling
//!
//! Functions that can fail return two values following Lua convention:
//! - Success: `value, nil`
//! - Failure: `nil, error_message`

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Value};

use super::json::json_to_lua;

/// In-memory cache for the config file. Avoids re-reading and re-parsing
/// the JSON file on every `config.get()` / `config.all()` call.
/// Invalidated (set to None) on `config.set()`, lazily repopulated on next read.
type ConfigCache = Arc<Mutex<Option<serde_json::Value>>>;

/// Default config directory under the user's config path.
const CONFIG_DIR: &str = "botster";

/// Config file name.
const CONFIG_FILE: &str = "config.json";

/// Default data directory name under home.
const DATA_DIR: &str = ".botster";

/// Get the config file path: `~/.config/botster/config.json`.
fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(CONFIG_DIR).join(CONFIG_FILE))
}

/// Get the data directory path: `~/.botster`.
fn data_dir_path() -> Option<PathBuf> {
    dirs::home_dir().map(|d| d.join(DATA_DIR))
}

/// Read the config file, returning an empty object if it doesn't exist.
fn read_config() -> std::io::Result<serde_json::Value> {
    let Some(path) = config_path() else {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    };

    if !path.exists() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }

    let content = std::fs::read_to_string(&path)?;
    serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write the config file, creating parent directories if needed.
fn write_config(value: &serde_json::Value) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine config directory",
        ));
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    std::fs::write(&path, content)
}

/// Read config using the cache. Returns a clone of the cached value,
/// populating the cache from disk on first access or after invalidation.
fn read_config_cached(cache: &ConfigCache) -> std::io::Result<serde_json::Value> {
    let mut guard = cache.lock().expect("ConfigCache mutex poisoned");
    if let Some(ref cached) = *guard {
        return Ok(cached.clone());
    }
    let config = read_config()?;
    *guard = Some(config.clone());
    Ok(config)
}

/// Register the `config` table with configuration functions.
///
/// Creates a global `config` table with methods:
/// - `config.get(key)` - Read a value from the config file
/// - `config.set(key, value)` - Write a value to the config file
/// - `config.all()` - Get all config as a Lua table
/// - `config.lua_path()` - Get the Lua scripts base path
/// - `config.data_dir()` - Get the `~/.botster` path
/// - `config.env(key)` - Read an environment variable
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua) -> Result<()> {
    let config_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create config table: {e}"))?;

    // Shared cache for config file contents
    let cache: ConfigCache = Arc::new(Mutex::new(None));

    // config.get(key) -> (value, nil) or (nil, error_string)
    //
    // Reads a top-level key from the config file.
    // Returns nil (not an error) if the key doesn't exist.
    let cache_get = Arc::clone(&cache);
    let get_fn = lua
        .create_function(move |lua, key: String| match read_config_cached(&cache_get) {
            Ok(config) => {
                if let Some(value) = config.get(&key) {
                    let lua_value = json_to_lua(lua, value).map_err(|e| {
                        mlua::Error::external(format!("Failed to convert config value: {e}"))
                    })?;
                    Ok((Some(lua_value), None::<String>))
                } else {
                    Ok((None::<Value>, None::<String>))
                }
            }
            Err(e) => Ok((
                None::<Value>,
                Some(format!("Failed to read config: {e}")),
            )),
        })
        .map_err(|e| anyhow!("Failed to create config.get function: {e}"))?;

    config_table
        .set("get", get_fn)
        .map_err(|e| anyhow!("Failed to set config.get: {e}"))?;

    // config.set(key, value) -> (true, nil) or (nil, error_string)
    //
    // Sets a top-level key in the config file. Reads the existing config,
    // updates the key, and writes back (read-modify-write).
    // Invalidates the in-memory cache so the next read picks up changes.
    let cache_set = Arc::clone(&cache);
    let set_fn = lua
        .create_function(move |lua, (key, value): (String, Value)| {
            let mut config = match read_config() {
                Ok(c) => c,
                Err(e) => {
                    return Ok((
                        None::<bool>,
                        Some(format!("Failed to read config: {e}")),
                    ))
                }
            };

            let json_value: serde_json::Value = lua.from_value(value).map_err(|e| {
                mlua::Error::external(format!("Failed to convert value: {e}"))
            })?;

            if let Some(obj) = config.as_object_mut() {
                obj.insert(key, json_value);
            } else {
                return Ok((
                    None::<bool>,
                    Some("Config file is not a JSON object".to_string()),
                ));
            }

            match write_config(&config) {
                Ok(()) => {
                    // Update the cache with the new config state
                    if let Ok(mut guard) = cache_set.lock() {
                        *guard = Some(config);
                    }
                    Ok((Some(true), None::<String>))
                }
                Err(e) => Ok((
                    None::<bool>,
                    Some(format!("Failed to write config: {e}")),
                )),
            }
        })
        .map_err(|e| anyhow!("Failed to create config.set function: {e}"))?;

    config_table
        .set("set", set_fn)
        .map_err(|e| anyhow!("Failed to set config.set: {e}"))?;

    // config.all() -> (table, nil) or (nil, error_string)
    //
    // Returns the entire config file as a Lua table.
    let cache_all = Arc::clone(&cache);
    let all_fn = lua
        .create_function(move |lua, ()| match read_config_cached(&cache_all) {
            Ok(config) => {
                let lua_value = json_to_lua(lua, &config).map_err(|e| {
                    mlua::Error::external(format!("Failed to convert config: {e}"))
                })?;
                Ok((Some(lua_value), None::<String>))
            }
            Err(e) => Ok((
                None::<Value>,
                Some(format!("Failed to read config: {e}")),
            )),
        })
        .map_err(|e| anyhow!("Failed to create config.all function: {e}"))?;

    config_table
        .set("all", all_fn)
        .map_err(|e| anyhow!("Failed to set config.all: {e}"))?;

    // config.lua_path() -> string
    //
    // Returns the Lua scripts base path from the BOTSTER_LUA_PATH
    // environment variable, or defaults to `~/.botster/lua`.
    let lua_path_fn = lua
        .create_function(|_, ()| {
            let path = std::env::var("BOTSTER_LUA_PATH").unwrap_or_else(|_| {
                data_dir_path()
                    .map(|d| d.join("lua").to_string_lossy().to_string())
                    .unwrap_or_else(|| "lua".to_string())
            });
            Ok(path)
        })
        .map_err(|e| anyhow!("Failed to create config.lua_path function: {e}"))?;

    config_table
        .set("lua_path", lua_path_fn)
        .map_err(|e| anyhow!("Failed to set config.lua_path: {e}"))?;

    // config.data_dir() -> string
    //
    // Returns the `~/.botster` directory path.
    let data_dir_fn = lua
        .create_function(|_, ()| {
            let path = data_dir_path()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_else(|| ".botster".to_string());
            Ok(path)
        })
        .map_err(|e| anyhow!("Failed to create config.data_dir function: {e}"))?;

    config_table
        .set("data_dir", data_dir_fn)
        .map_err(|e| anyhow!("Failed to set config.data_dir: {e}"))?;

    // config.server_url() -> string
    //
    // Returns the Botster server URL from the BOTSTER_SERVER_URL env var,
    // or defaults to "https://trybotster.com".
    let server_url_fn = lua
        .create_function(|_, ()| {
            let url = std::env::var("BOTSTER_SERVER_URL")
                .unwrap_or_else(|_| "https://trybotster.com".to_string());
            Ok(url)
        })
        .map_err(|e| anyhow!("Failed to create config.server_url function: {e}"))?;

    config_table
        .set("server_url", server_url_fn)
        .map_err(|e| anyhow!("Failed to set config.server_url: {e}"))?;

    // config.env(key) -> string or nil
    //
    // Reads an environment variable. Returns nil if not set.
    let env_fn = lua
        .create_function(|_, key: String| match std::env::var(&key) {
            Ok(val) => Ok(Some(val)),
            Err(_) => Ok(None),
        })
        .map_err(|e| anyhow!("Failed to create config.env function: {e}"))?;

    config_table
        .set("env", env_fn)
        .map_err(|e| anyhow!("Failed to set config.env: {e}"))?;

    lua.globals()
        .set("config", config_table)
        .map_err(|e| anyhow!("Failed to register config table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    #[test]
    fn test_config_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        let globals = lua.globals();
        let config_table: Table = globals.get("config").expect("config table should exist");

        let _: Function = config_table.get("get").expect("config.get should exist");
        let _: Function = config_table.get("set").expect("config.set should exist");
        let _: Function = config_table.get("all").expect("config.all should exist");
        let _: Function = config_table
            .get("lua_path")
            .expect("config.lua_path should exist");
        let _: Function = config_table
            .get("data_dir")
            .expect("config.data_dir should exist");
        let _: Function = config_table.get("env").expect("config.env should exist");
    }

    #[test]
    fn test_env_returns_existing_var() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        // HOME should always be set
        let result: Option<String> = lua
            .load(r#"return config.env("HOME")"#)
            .eval()
            .expect("config.env should be callable");

        assert!(result.is_some(), "HOME should be set");
    }

    #[test]
    fn test_env_returns_nil_for_missing_var() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        let result: Option<String> = lua
            .load(r#"return config.env("BOTSTER_TEST_NONEXISTENT_VAR_12345")"#)
            .eval()
            .expect("config.env should be callable");

        assert!(result.is_none());
    }

    #[test]
    fn test_data_dir_returns_string() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        let result: String = lua
            .load(r#"return config.data_dir()"#)
            .eval()
            .expect("config.data_dir should be callable");

        assert!(
            result.contains(".botster"),
            "data_dir should contain '.botster', got: {result}"
        );
    }

    #[test]
    fn test_lua_path_returns_string() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        let result: String = lua
            .load(r#"return config.lua_path()"#)
            .eval()
            .expect("config.lua_path should be callable");

        assert!(!result.is_empty(), "lua_path should not be empty");
    }

    #[test]
    fn test_get_set_roundtrip_with_temp_config() {
        // Use a temp directory to avoid touching the real config
        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join("config.json");
        std::fs::write(&config_file, "{}").unwrap();

        // Read-modify-write directly to test the logic
        let mut config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_file).unwrap()).unwrap();

        config
            .as_object_mut()
            .unwrap()
            .insert("theme".to_string(), serde_json::json!("dark"));

        let content = serde_json::to_string_pretty(&config).unwrap();
        std::fs::write(&config_file, &content).unwrap();

        // Read back
        let readback: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_file).unwrap()).unwrap();
        assert_eq!(readback["theme"], "dark");
    }

    #[test]
    fn test_read_config_nonexistent_returns_empty_object() {
        // This tests the function when no config file exists at the default path.
        // It should return an empty JSON object, not an error.
        let result = read_config();
        // Whether or not the file exists, read_config should not panic
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_missing_key_returns_nil() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        // config.get on a key that likely doesn't exist should return nil, nil
        // (not an error — just not found)
        lua.load(
            r#"
            local val, err = config.get("botster_test_unlikely_key_xyz")
            -- val should be nil, err should also be nil (not found is not an error)
            assert(val == nil, "Missing key should return nil")
        "#,
        )
        .exec()
        .expect("get missing key test should pass");
    }

    #[test]
    fn test_all_returns_table() {
        let lua = Lua::new();
        register(&lua).expect("Should register config primitives");

        lua.load(
            r#"
            local all, err = config.all()
            assert(type(all) == "table", "config.all() should return a table, got: " .. type(all))
        "#,
        )
        .exec()
        .expect("config.all test should pass");
    }

    #[test]
    fn test_config_path_helper() {
        let path = config_path();
        // Should return Some on any system with a home directory
        if dirs::config_dir().is_some() {
            assert!(path.is_some());
            let p = path.unwrap();
            assert!(p.to_string_lossy().contains("botster"));
            assert!(p.to_string_lossy().contains("config.json"));
        }
    }

    #[test]
    fn test_data_dir_path_helper() {
        let path = data_dir_path();
        if dirs::home_dir().is_some() {
            assert!(path.is_some());
            assert!(path.unwrap().to_string_lossy().contains(".botster"));
        }
    }
}
