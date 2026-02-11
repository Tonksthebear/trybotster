//! JSON encode/decode primitives for Lua scripts.
//!
//! Exposes explicit JSON serialization and deserialization to Lua,
//! allowing scripts to convert between Lua tables and JSON strings.
//!
//! # Design
//!
//! All operations are synchronous and use `mlua::LuaSerdeExt` for
//! Lua <-> serde_json conversion. No queues are needed.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Encode a Lua table to JSON
//! local str, err = json.encode({ name = "bot", version = 1 })
//! if str then
//!     log.info("JSON: " .. str)
//! end
//!
//! -- Decode a JSON string to a Lua table
//! local data, err = json.decode('{"name":"bot","version":1}')
//! if data then
//!     log.info("Name: " .. data.name)
//! end
//!
//! -- Pretty-print JSON
//! local pretty, err = json.encode_pretty({ name = "bot" })
//! ```
//!
//! # Error Handling
//!
//! Functions that can fail return two values following Lua convention:
//! - Success: `value, nil`
//! - Failure: `nil, error_message`

use anyhow::{anyhow, Result};
use mlua::{Lua, LuaSerdeExt, Value};

/// Convert a serde_json::Value to a Lua value, mapping JSON null to Lua nil.
///
/// The default `LuaSerdeExt::to_value` maps JSON null to a light-userdata
/// sentinel (`Value::NULL`), which is truthy in Lua. This function instead
/// produces real `nil`, which matches Lua convention.
///
/// Used by both `json.decode()` and `config.get()` / `config.all()`.
pub fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    match v {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Ok(Value::Nil)
            }
        }
        serde_json::Value::String(s) => lua.create_string(s).map(Value::String),
        serde_json::Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, item) in arr.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, item)?)?;
            }
            Ok(Value::Table(table))
        }
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (key, val) in map {
                // Skip null values entirely — they become absent keys (nil)
                if !val.is_null() {
                    table.set(lua.create_string(key)?, json_to_lua(lua, val)?)?;
                }
            }
            Ok(Value::Table(table))
        }
    }
}

/// Register the `json` table with encode/decode functions.
///
/// Creates a global `json` table with methods:
/// - `json.encode(value)` - Serialize any Lua value to a JSON string
/// - `json.decode(string)` - Deserialize a JSON string to a Lua value
/// - `json.encode_pretty(value)` - Serialize to a pretty-printed JSON string
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua) -> Result<()> {
    let json_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create json table: {e}"))?;

    // json.encode(value) -> (string, nil) or (nil, error_string)
    //
    // Serializes any Lua value (table, string, number, boolean, nil)
    // to a compact JSON string.
    let encode_fn = lua
        .create_function(|lua, value: Value| {
            let json_value: serde_json::Value = lua.from_value(value).map_err(|e| {
                mlua::Error::external(format!("Failed to convert Lua value: {e}"))
            })?;

            match serde_json::to_string(&json_value) {
                Ok(s) => Ok((Some(s), None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("Failed to encode JSON: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create json.encode function: {e}"))?;

    json_table
        .set("encode", encode_fn)
        .map_err(|e| anyhow!("Failed to set json.encode: {e}"))?;

    // json.decode(string) -> (value, nil) or (nil, error_string)
    //
    // Deserializes a JSON string into a Lua value (table, string,
    // number, boolean, or nil).
    let decode_fn = lua
        .create_function(|lua, s: String| {
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(json_value) => {
                    let lua_value = json_to_lua(lua, &json_value).map_err(|e| {
                        mlua::Error::external(format!("Failed to convert to Lua value: {e}"))
                    })?;
                    Ok((Some(lua_value), None::<String>))
                }
                Err(e) => Ok((None::<Value>, Some(format!("Failed to decode JSON: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create json.decode function: {e}"))?;

    json_table
        .set("decode", decode_fn)
        .map_err(|e| anyhow!("Failed to set json.decode: {e}"))?;

    // json.encode_pretty(value) -> (string, nil) or (nil, error_string)
    //
    // Same as encode but produces indented, human-readable JSON output.
    let encode_pretty_fn = lua
        .create_function(|lua, value: Value| {
            let json_value: serde_json::Value = lua.from_value(value).map_err(|e| {
                mlua::Error::external(format!("Failed to convert Lua value: {e}"))
            })?;

            match serde_json::to_string_pretty(&json_value) {
                Ok(s) => Ok((Some(s), None::<String>)),
                Err(e) => Ok((
                    None::<String>,
                    Some(format!("Failed to encode JSON: {e}")),
                )),
            }
        })
        .map_err(|e| anyhow!("Failed to create json.encode_pretty function: {e}"))?;

    json_table
        .set("encode_pretty", encode_pretty_fn)
        .map_err(|e| anyhow!("Failed to set json.encode_pretty: {e}"))?;

    lua.globals()
        .set("json", json_table)
        .map_err(|e| anyhow!("Failed to register json table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    #[test]
    fn test_json_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let globals = lua.globals();
        let json_table: Table = globals.get("json").expect("json table should exist");

        let _: Function = json_table.get("encode").expect("json.encode should exist");
        let _: Function = json_table.get("decode").expect("json.decode should exist");
        let _: Function = json_table
            .get("encode_pretty")
            .expect("json.encode_pretty should exist");
    }

    #[test]
    fn test_encode_table() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode({ name = "bot", version = 1 })"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        let s = result.expect("Should return a string");
        // Parse back to verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("Should be valid JSON");
        assert_eq!(parsed["name"], "bot");
        assert_eq!(parsed["version"], 1);
    }

    #[test]
    fn test_encode_string() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode("hello")"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        assert_eq!(result, Some(r#""hello""#.to_string()));
    }

    #[test]
    fn test_encode_number() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode(42)"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        assert_eq!(result, Some("42".to_string()));
    }

    #[test]
    fn test_encode_boolean() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode(true)"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn test_encode_array() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode({1, 2, 3})"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        let s = result.expect("Should return a string");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("Should be valid JSON");
        assert_eq!(parsed, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_decode_object() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        lua.load(
            r#"
            local data, err = json.decode('{"name":"bot","version":1}')
            assert(err == nil, "Should not error")
            assert(data.name == "bot", "name should be 'bot'")
            assert(data.version == 1, "version should be 1")
        "#,
        )
        .exec()
        .expect("decode object test should pass");
    }

    #[test]
    fn test_decode_array() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        lua.load(
            r#"
            local data, err = json.decode('[1, 2, 3]')
            assert(err == nil, "Should not error")
            assert(#data == 3, "Should have 3 elements")
            assert(data[1] == 1)
            assert(data[2] == 2)
            assert(data[3] == 3)
        "#,
        )
        .exec()
        .expect("decode array test should pass");
    }

    #[test]
    fn test_decode_invalid_json_returns_error() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (value, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.decode("not valid json {")"#)
            .eval()
            .expect("json.decode should be callable");

        assert!(value.is_none());
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("Failed to decode JSON"),
            "Error should describe the failure"
        );
    }

    #[test]
    fn test_roundtrip_encode_decode() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        lua.load(
            r#"
            local original = { name = "test", items = {1, 2, 3}, active = true }
            local encoded, err1 = json.encode(original)
            assert(err1 == nil, "encode should not error")

            local decoded, err2 = json.decode(encoded)
            assert(err2 == nil, "decode should not error")
            assert(decoded.name == "test")
            assert(decoded.active == true)
            assert(#decoded.items == 3)
        "#,
        )
        .exec()
        .expect("roundtrip test should pass");
    }

    #[test]
    fn test_encode_pretty_output() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode_pretty({ name = "bot" })"#)
            .eval()
            .expect("json.encode_pretty should be callable");

        assert!(err.is_none());
        let s = result.expect("Should return a string");
        assert!(s.contains('\n'), "Pretty output should contain newlines");
        assert!(s.contains("  "), "Pretty output should contain indentation");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("Should be valid JSON");
        assert_eq!(parsed["name"], "bot");
    }

    #[test]
    fn test_encode_nested_table() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        let (result, err): (Option<String>, Option<String>) = lua
            .load(r#"return json.encode({ outer = { inner = "value" } })"#)
            .eval()
            .expect("json.encode should be callable");

        assert!(err.is_none());
        let s = result.expect("Should return a string");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("Should be valid JSON");
        assert_eq!(parsed["outer"]["inner"], "value");
    }

    #[test]
    fn test_decode_null_becomes_nil() {
        let lua = Lua::new();
        register(&lua).expect("Should register json primitives");

        lua.load(
            r#"
            local data, err = json.decode('{"key": null}')
            assert(err == nil, "Should not error")
            assert(data.key == nil, "null should become nil")
        "#,
        )
        .exec()
        .expect("null decode test should pass");
    }
}
