//! File system primitives for Lua scripts.
//!
//! Exposes synchronous file operations to Lua, allowing scripts to read,
//! write, copy, and check existence of files on the local file system.
//!
//! # Design
//!
//! All operations are synchronous and run directly in the Lua callback.
//! No queues are needed since these are simple blocking I/O operations
//! that complete immediately.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Write a file (creates parent directories if needed)
//! local ok, err = fs.write("/tmp/output.txt", "Hello, world!")
//! if not ok then
//!     log.error("Write failed: " .. err)
//! end
//!
//! -- Read a file
//! local content, err = fs.read("/tmp/output.txt")
//! if content then
//!     log.info("File contents: " .. content)
//! else
//!     log.error("Read failed: " .. err)
//! end
//!
//! -- Check if a file exists
//! if fs.exists("/tmp/output.txt") then
//!     log.info("File exists")
//! end
//!
//! -- Copy a file
//! local ok, err = fs.copy("/tmp/output.txt", "/tmp/backup.txt")
//! if not ok then
//!     log.error("Copy failed: " .. err)
//! end
//! ```
//!
//! # Error Handling
//!
//! Functions that can fail return two values following Lua convention:
//! - Success: `value, nil`
//! - Failure: `nil, error_message`

use std::path::Path;

use anyhow::{anyhow, Result};
use mlua::Lua;

/// Register the `fs` table with file system functions.
///
/// Creates a global `fs` table with methods:
/// - `fs.write(path, content)` - Write string content to a file (creates parent dirs)
/// - `fs.read(path)` - Read file contents as a string
/// - `fs.exists(path)` - Check if a file exists
/// - `fs.copy(src, dst)` - Copy a file
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua) -> Result<()> {
    let fs_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create fs table: {e}"))?;

    // fs.write(path, content) -> (true, nil) or (nil, error_string)
    //
    // Writes string content to a file at the given path.
    // Creates parent directories if they don't exist.
    let write_fn = lua
        .create_function(|_, (path, content): (String, String)| {
            let file_path = Path::new(&path);

            // Create parent directories if needed
            if let Some(parent) = file_path.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return Ok((None::<bool>, Some(format!("Failed to create parent directories: {e}"))));
                    }
                }
            }

            match std::fs::write(file_path, content) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("Failed to write file: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.write function: {e}"))?;

    fs_table
        .set("write", write_fn)
        .map_err(|e| anyhow!("Failed to set fs.write: {e}"))?;

    // fs.read(path) -> (content, nil) or (nil, error_string)
    //
    // Reads the entire file contents as a UTF-8 string.
    let read_fn = lua
        .create_function(|_, path: String| {
            match std::fs::read_to_string(&path) {
                Ok(content) => Ok((Some(content), None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("Failed to read file: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.read function: {e}"))?;

    fs_table
        .set("read", read_fn)
        .map_err(|e| anyhow!("Failed to set fs.read: {e}"))?;

    // fs.exists(path) -> boolean
    //
    // Returns true if the file or directory exists at the given path.
    let exists_fn = lua
        .create_function(|_, path: String| Ok(Path::new(&path).exists()))
        .map_err(|e| anyhow!("Failed to create fs.exists function: {e}"))?;

    fs_table
        .set("exists", exists_fn)
        .map_err(|e| anyhow!("Failed to set fs.exists: {e}"))?;

    // fs.copy(src, dst) -> (true, nil) or (nil, error_string)
    //
    // Copies a file from src to dst. Overwrites dst if it already exists.
    let copy_fn = lua
        .create_function(|_, (src, dst): (String, String)| {
            match std::fs::copy(&src, &dst) {
                Ok(_) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("Failed to copy file: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.copy function: {e}"))?;

    fs_table
        .set("copy", copy_fn)
        .map_err(|e| anyhow!("Failed to set fs.copy: {e}"))?;

    // Register the table globally
    lua.globals()
        .set("fs", fs_table)
        .map_err(|e| anyhow!("Failed to register fs table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_fs_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let globals = lua.globals();
        let fs_table: Table = globals.get("fs").expect("fs table should exist");

        // Verify all functions exist
        let _: Function = fs_table.get("write").expect("fs.write should exist");
        let _: Function = fs_table.get("read").expect("fs.read should exist");
        let _: Function = fs_table.get("exists").expect("fs.exists should exist");
        let _: Function = fs_table.get("copy").expect("fs.copy should exist");
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let path_str = path.to_str().unwrap();

        // Write a file
        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(r#"return fs.write("{path_str}", "hello world")"#))
            .eval()
            .expect("fs.write should be callable");

        assert_eq!(ok, Some(true));
        assert!(err.is_none());

        // Read it back
        let (content, err): (Option<String>, Option<String>) = lua
            .load(format!(r#"return fs.read("{path_str}")"#))
            .eval()
            .expect("fs.read should be callable");

        assert_eq!(content, Some("hello world".to_string()));
        assert!(err.is_none());
    }

    #[test]
    fn test_write_creates_parent_directories() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("file.txt");
        let path_str = path.to_str().unwrap();

        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(r#"return fs.write("{path_str}", "nested content")"#))
            .eval()
            .expect("fs.write should be callable");

        assert_eq!(ok, Some(true));
        assert!(err.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested content");
    }

    #[test]
    fn test_read_nonexistent_file_returns_error() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let (content, err): (Option<String>, Option<String>) = lua
            .load(r#"return fs.read("/nonexistent/path/file.txt")"#)
            .eval()
            .expect("fs.read should be callable");

        assert!(content.is_none());
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("Failed to read file"),
            "Error should describe the failure"
        );
    }

    #[test]
    fn test_exists_returns_true_for_existing_file() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "test").unwrap();
        let path_str = tmp.path().to_str().unwrap();

        let exists: bool = lua
            .load(format!(r#"return fs.exists("{path_str}")"#))
            .eval()
            .expect("fs.exists should be callable");

        assert!(exists);
    }

    #[test]
    fn test_exists_returns_false_for_nonexistent_file() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let exists: bool = lua
            .load(r#"return fs.exists("/nonexistent/path/file.txt")"#)
            .eval()
            .expect("fs.exists should be callable");

        assert!(!exists);
    }

    #[test]
    fn test_copy_file() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source.txt");
        let dst = dir.path().join("dest.txt");

        std::fs::write(&src, "copy me").unwrap();

        let src_str = src.to_str().unwrap();
        let dst_str = dst.to_str().unwrap();

        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(r#"return fs.copy("{src_str}", "{dst_str}")"#))
            .eval()
            .expect("fs.copy should be callable");

        assert_eq!(ok, Some(true));
        assert!(err.is_none());
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "copy me");
    }

    #[test]
    fn test_copy_nonexistent_source_returns_error() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("dest.txt");
        let dst_str = dst.to_str().unwrap();

        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(
                r#"return fs.copy("/nonexistent/source.txt", "{dst_str}")"#
            ))
            .eval()
            .expect("fs.copy should be callable");

        assert!(ok.is_none());
        assert!(err.is_some());
        assert!(
            err.unwrap().contains("Failed to copy file"),
            "Error should describe the failure"
        );
    }
}
