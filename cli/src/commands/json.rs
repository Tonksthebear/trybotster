//! JSON file manipulation commands.
//!
//! Provides CLI utilities for reading, modifying, and deleting values in JSON
//! files using dot-notation paths. Useful for managing configuration files
//! like Claude's `projects.json`.
//!
//! # Path Notation
//!
//! Keys are specified using dot notation: `projects.myproject.hasTrust`
//!
//! # Examples
//!
//! ```bash
//! # Get a value
//! botster-hub json-get ~/.config/claude/projects.json "projects.myproject.hasTrust"
//!
//! # Set a value (creates intermediate objects if needed)
//! botster-hub json-set ~/.config/claude/projects.json "projects.myproject.hasTrust" "true"
//!
//! # Delete a key
//! botster-hub json-delete ~/.config/claude/projects.json "projects.myproject.hasTrust"
//! ```

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Reads a value from a JSON file using dot-notation path.
///
/// Navigates through the JSON structure using the provided key path and prints
/// the resulting value as pretty-printed JSON to stdout.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read
/// - The file contains invalid JSON
/// - Any key in the path does not exist
///
/// # Examples
///
/// ```ignore
/// // Read "projects.myproject.hasTrust" from a JSON file
/// json::get("~/.config/claude/projects.json", "projects.myproject.hasTrust")?;
/// ```
pub fn get(file_path: &str, key_path: &str) -> Result<()> {
    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {} as JSON", file_path))?;

    // Navigate through the key path
    for key in key_path.split('.') {
        value = value
            .get(key)
            .with_context(|| format!("Key '{}' not found in path '{}'", key, key_path))?
            .clone();
    }

    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Sets a value in a JSON file using dot-notation path.
///
/// Navigates to the specified location in the JSON structure and sets the value.
/// Creates intermediate objects if they don't exist. The value is parsed as JSON
/// first; if parsing fails, it's treated as a string.
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read or written
/// - The file contains invalid JSON
/// - An intermediate key exists but is not an object
///
/// # Examples
///
/// ```ignore
/// // Set a boolean value
/// json::set("config.json", "settings.enabled", "true")?;
///
/// // Set an object value
/// json::set("config.json", "settings.options", r#"{"key": "value"}"#)?;
/// ```
pub fn set(file_path: &str, key_path: &str, new_value: &str) -> Result<()> {
    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {} as JSON", file_path))?;

    // Parse the new value as JSON, fall back to string if parsing fails
    let parsed_value: serde_json::Value = serde_json::from_str(new_value)
        .unwrap_or_else(|_| serde_json::Value::String(new_value.to_string()));

    // Split the path and navigate/create structure
    let keys: Vec<&str> = key_path.split('.').collect();
    let mut current = &mut root;

    for (i, key) in keys.iter().enumerate() {
        if i == keys.len() - 1 {
            // Last key - set the value
            if let Some(obj) = current.as_object_mut() {
                obj.insert(key.to_string(), parsed_value.clone());
            } else {
                anyhow::bail!("Cannot set key '{}' - parent is not an object", key);
            }
        } else {
            // Navigate/create intermediate objects
            if !current.is_object() {
                anyhow::bail!("Cannot navigate through '{}' - not an object", key);
            }

            let obj = current.as_object_mut().expect("checked is_object() above");

            // If key doesn't exist or exists but isn't an object, create/replace with empty object
            if !obj.contains_key(*key) || !obj[*key].is_object() {
                obj.insert(key.to_string(), serde_json::json!({}));
            }
            current = obj.get_mut(*key).expect("key was just inserted if missing");
        }
    }

    // Write back to file with pretty formatting
    fs::write(
        Path::new(path.as_ref()),
        serde_json::to_string_pretty(&root)?,
    )
    .with_context(|| format!("Failed to write {}", file_path))?;

    Ok(())
}

/// Deletes a key from a JSON file using dot-notation path.
///
/// Navigates to the parent of the specified key and removes it. If any
/// intermediate key doesn't exist, the operation succeeds silently (idempotent).
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be read or written
/// - The file contains invalid JSON
/// - Attempting to delete the root object
/// - An intermediate key is not an object
///
/// # Examples
///
/// ```ignore
/// // Delete a nested key
/// json::delete("config.json", "settings.deprecated_option")?;
/// ```
pub fn delete(file_path: &str, key_path: &str) -> Result<()> {
    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {} as JSON", file_path))?;

    // Split the path and navigate to parent
    let keys: Vec<&str> = key_path.split('.').collect();
    // Empty path or single empty key both indicate root deletion attempt
    if keys.is_empty() || (keys.len() == 1 && keys[0].is_empty()) {
        anyhow::bail!("Cannot delete root object");
    }

    let mut current = &mut root;

    // Navigate to the parent of the key we want to delete
    for (i, key) in keys.iter().enumerate() {
        if i == keys.len() - 1 {
            // Last key - delete it
            if let Some(obj) = current.as_object_mut() {
                obj.remove(*key);
            } else {
                anyhow::bail!("Cannot delete key '{}' - parent is not an object", key);
            }
        } else {
            // Navigate to next level
            if !current.is_object() {
                anyhow::bail!("Cannot navigate through '{}' - not an object", key);
            }

            let obj = current.as_object_mut().expect("checked is_object() above");
            if !obj.contains_key(*key) {
                // Key doesn't exist, nothing to delete (idempotent)
                return Ok(());
            }

            current = obj.get_mut(*key).expect("checked contains_key() above");
        }
    }

    // Write back to file with pretty formatting
    fs::write(
        Path::new(path.as_ref()),
        serde_json::to_string_pretty(&root)?,
    )
    .with_context(|| format!("Failed to write {}", file_path))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        file
    }

    #[test]
    fn test_get_simple_key() {
        let file = create_test_file(r#"{"name": "test", "value": 42}"#);
        let path = file.path().to_str().unwrap();

        // Should succeed without panicking
        let result = get(path, "name");
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_nested_key() {
        let file = create_test_file(r#"{"outer": {"inner": {"deep": "found"}}}"#);
        let path = file.path().to_str().unwrap();

        let result = get(path, "outer.inner.deep");
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_missing_key() {
        let file = create_test_file(r#"{"name": "test"}"#);
        let path = file.path().to_str().unwrap();

        let result = get(path, "missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_set_simple_value() {
        let file = create_test_file(r#"{"name": "old"}"#);
        let path = file.path().to_str().unwrap();

        set(path, "name", "\"new\"").unwrap();

        let content = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["name"], "new");
    }

    #[test]
    fn test_set_creates_intermediate_objects() {
        let file = create_test_file(r#"{}"#);
        let path = file.path().to_str().unwrap();

        set(path, "a.b.c", "\"deep\"").unwrap();

        let content = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["a"]["b"]["c"], "deep");
    }

    #[test]
    fn test_set_boolean_value() {
        let file = create_test_file(r#"{"enabled": false}"#);
        let path = file.path().to_str().unwrap();

        set(path, "enabled", "true").unwrap();

        let content = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["enabled"], true);
    }

    #[test]
    fn test_delete_key() {
        let file = create_test_file(r#"{"keep": 1, "remove": 2}"#);
        let path = file.path().to_str().unwrap();

        delete(path, "remove").unwrap();

        let content = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed.get("keep").is_some());
        assert!(parsed.get("remove").is_none());
    }

    #[test]
    fn test_delete_nested_key() {
        let file = create_test_file(r#"{"outer": {"keep": 1, "remove": 2}}"#);
        let path = file.path().to_str().unwrap();

        delete(path, "outer.remove").unwrap();

        let content = fs::read_to_string(path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["outer"].get("keep").is_some());
        assert!(parsed["outer"].get("remove").is_none());
    }

    #[test]
    fn test_delete_missing_key_is_idempotent() {
        let file = create_test_file(r#"{"name": "test"}"#);
        let path = file.path().to_str().unwrap();

        // Should succeed even though key doesn't exist
        let result = delete(path, "nonexistent");
        assert!(result.is_ok());
    }

    #[test]
    fn test_delete_root_fails() {
        let file = create_test_file(r#"{"name": "test"}"#);
        let path = file.path().to_str().unwrap();

        let result = delete(path, "");
        assert!(result.is_err());
    }
}
