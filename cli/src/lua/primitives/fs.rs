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
/// - `fs.listdir(path)` - List entries in a directory
/// - `fs.is_dir(path)` - Check if path is a directory
/// - `fs.rmdir(path)` - Recursively remove a directory and all contents
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

    // fs.listdir(path) -> (entries, nil) or (nil, error_string)
    //
    // Returns an array of entry names (files and directories) in the given directory.
    // Does not include "." or "..".
    let listdir_fn = lua
        .create_function(|lua, path: String| {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    let table = lua.create_table()?;
                    let mut i = 1;
                    for entry in entries.flatten() {
                        if let Some(name) = entry.file_name().to_str() {
                            table.set(i, name.to_string())?;
                            i += 1;
                        }
                    }
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<mlua::Table>, Some(format!("Failed to list directory: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.listdir function: {e}"))?;

    fs_table
        .set("listdir", listdir_fn)
        .map_err(|e| anyhow!("Failed to set fs.listdir: {e}"))?;

    // fs.is_dir(path) -> boolean
    //
    // Returns true if the path exists and is a directory.
    let is_dir_fn = lua
        .create_function(|_, path: String| Ok(Path::new(&path).is_dir()))
        .map_err(|e| anyhow!("Failed to create fs.is_dir function: {e}"))?;

    fs_table
        .set("is_dir", is_dir_fn)
        .map_err(|e| anyhow!("Failed to set fs.is_dir: {e}"))?;

    // fs.delete(path) -> (true, nil) or (nil, error_string)
    //
    // Deletes a file or empty directory at the given path.
    let delete_fn = lua
        .create_function(|_, path: String| {
            let p = Path::new(&path);
            if !p.exists() {
                return Ok((None::<bool>, Some("Path does not exist".to_string())));
            }
            let result = if p.is_dir() {
                std::fs::remove_dir(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match result {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("Failed to delete: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.delete function: {e}"))?;

    fs_table
        .set("delete", delete_fn)
        .map_err(|e| anyhow!("Failed to set fs.delete: {e}"))?;

    // fs.mkdir(path) -> (true, nil) or (nil, error_string)
    //
    // Creates a directory and all parent directories.
    let mkdir_fn = lua
        .create_function(|_, path: String| {
            match std::fs::create_dir_all(&path) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("Failed to create directory: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.mkdir function: {e}"))?;

    fs_table
        .set("mkdir", mkdir_fn)
        .map_err(|e| anyhow!("Failed to set fs.mkdir: {e}"))?;

    // fs.rmdir(path) -> (true, nil) or (nil, error_string)
    //
    // Recursively removes a directory and all of its contents.
    // Fails if the path does not exist or is not a directory.
    let rmdir_fn = lua
        .create_function(|_, path: String| {
            let p = Path::new(&path);
            if !p.exists() {
                return Ok((None::<bool>, Some("Path does not exist".to_string())));
            }
            if !p.is_dir() {
                return Ok((None::<bool>, Some("Path is not a directory".to_string())));
            }
            match std::fs::remove_dir_all(&path) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => {
                    Ok((None::<bool>, Some(format!("Failed to remove directory: {e}"))))
                }
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.rmdir function: {e}"))?;

    fs_table
        .set("rmdir", rmdir_fn)
        .map_err(|e| anyhow!("Failed to set fs.rmdir: {e}"))?;

    // fs.stat(path) -> (table, nil) or (nil, error_string)
    //
    // Returns { type = "file"|"dir", size = N, exists = true } or { exists = false }.
    let stat_fn = lua
        .create_function(|lua, path: String| {
            let p = Path::new(&path);
            if !p.exists() {
                let table = lua.create_table()?;
                table.set("exists", false)?;
                return Ok((Some(table), None::<String>));
            }
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let table = lua.create_table()?;
                    table.set("exists", true)?;
                    table.set(
                        "type",
                        if meta.is_dir() { "dir" } else { "file" },
                    )?;
                    table.set("size", meta.len())?;
                    Ok((Some(table), None::<String>))
                }
                Err(e) => Ok((None::<mlua::Table>, Some(format!("Failed to stat: {e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.stat function: {e}"))?;

    fs_table
        .set("stat", stat_fn)
        .map_err(|e| anyhow!("Failed to set fs.stat: {e}"))?;

    // fs.resolve_safe(root, relative) -> (absolute_path, nil) or (nil, error_string)
    //
    // Security primitive: canonicalizes path, verifies it stays within root.
    // Rejects absolute paths, `..` traversal, null bytes, symlink escapes.
    let resolve_safe_fn = lua
        .create_function(|_, (root, relative): (String, String)| {
            // Reject null bytes
            if relative.contains('\0') || root.contains('\0') {
                return Ok((None::<String>, Some("Path contains null byte".to_string())));
            }

            // Reject absolute paths in the relative component
            if relative.starts_with('/') || relative.starts_with('\\') {
                return Ok((
                    None::<String>,
                    Some("Absolute paths not allowed".to_string()),
                ));
            }

            // Reject explicit traversal patterns before canonicalization
            // (catches cases where the target doesn't exist yet)
            for component in relative.split('/') {
                if component == ".." {
                    return Ok((
                        None::<String>,
                        Some("Path traversal not allowed".to_string()),
                    ));
                }
            }

            let root_path = match std::fs::canonicalize(&root) {
                Ok(p) => p,
                Err(e) => {
                    return Ok((
                        None::<String>,
                        Some(format!("Failed to resolve root: {e}")),
                    ))
                }
            };

            let joined = root_path.join(&relative);

            // If the path exists, canonicalize to resolve symlinks
            let resolved = if joined.exists() {
                match std::fs::canonicalize(&joined) {
                    Ok(p) => p,
                    Err(e) => {
                        return Ok((
                            None::<String>,
                            Some(format!("Failed to resolve path: {e}")),
                        ))
                    }
                }
            } else {
                // For non-existent paths, normalize manually (no symlinks to resolve)
                // We already rejected ".." components above
                joined
            };

            // Verify the resolved path is within root
            if !resolved.starts_with(&root_path) {
                return Ok((
                    None::<String>,
                    Some("Path escapes root directory".to_string()),
                ));
            }

            match resolved.to_str() {
                Some(s) => Ok((Some(s.to_string()), None::<String>)),
                None => Ok((
                    None::<String>,
                    Some("Path contains invalid UTF-8".to_string()),
                )),
            }
        })
        .map_err(|e| anyhow!("Failed to create fs.resolve_safe function: {e}"))?;

    fs_table
        .set("resolve_safe", resolve_safe_fn)
        .map_err(|e| anyhow!("Failed to set fs.resolve_safe: {e}"))?;

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
    fn test_delete_file() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deleteme.txt");
        std::fs::write(&path, "bye").unwrap();
        assert!(path.exists());

        let path_str = path.to_str().unwrap();
        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(r#"return fs.delete("{path_str}")"#))
            .eval()
            .expect("fs.delete should be callable");

        assert_eq!(ok, Some(true));
        assert!(err.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn test_delete_nonexistent() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(r#"return fs.delete("/nonexistent/path/file.txt")"#)
            .eval()
            .expect("fs.delete should be callable");

        assert!(ok.is_none());
        assert!(err.is_some());
        assert!(err.unwrap().contains("does not exist"));
    }

    #[test]
    fn test_mkdir_creates_parents() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c");
        let path_str = path.to_str().unwrap();

        let (ok, err): (Option<bool>, Option<String>) = lua
            .load(format!(r#"return fs.mkdir("{path_str}")"#))
            .eval()
            .expect("fs.mkdir should be callable");

        assert_eq!(ok, Some(true));
        assert!(err.is_none());
        assert!(path.is_dir());
    }

    #[test]
    fn test_stat_file() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello").unwrap();
        let path_str = path.to_str().unwrap();

        let result: mlua::Table = lua
            .load(format!(r#"
                local stat, err = fs.stat("{path_str}")
                return stat
            "#))
            .eval()
            .expect("fs.stat should be callable");

        assert_eq!(result.get::<bool>("exists").unwrap(), true);
        assert_eq!(result.get::<String>("type").unwrap(), "file");
        assert_eq!(result.get::<u64>("size").unwrap(), 5);
    }

    #[test]
    fn test_stat_dir() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let path_str = dir.path().to_str().unwrap();

        let result: mlua::Table = lua
            .load(format!(r#"
                local stat, err = fs.stat("{path_str}")
                return stat
            "#))
            .eval()
            .expect("fs.stat should be callable");

        assert_eq!(result.get::<bool>("exists").unwrap(), true);
        assert_eq!(result.get::<String>("type").unwrap(), "dir");
    }

    #[test]
    fn test_stat_nonexistent() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let result: mlua::Table = lua
            .load(r#"
                local stat, err = fs.stat("/nonexistent/path/file.txt")
                return stat
            "#)
            .eval()
            .expect("fs.stat should be callable");

        assert_eq!(result.get::<bool>("exists").unwrap(), false);
    }

    #[test]
    fn test_resolve_safe_normal_path() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let root_str = dir.path().to_str().unwrap();

        // Create a file so canonicalize works
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        let (resolved, err): (Option<String>, Option<String>) = lua
            .load(format!(
                r#"return fs.resolve_safe("{root_str}", "test.txt")"#
            ))
            .eval()
            .expect("fs.resolve_safe should be callable");

        assert!(resolved.is_some());
        assert!(err.is_none());
        assert!(resolved.unwrap().ends_with("test.txt"));
    }

    #[test]
    fn test_resolve_safe_rejects_absolute() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let root_str = dir.path().to_str().unwrap();

        let (resolved, err): (Option<String>, Option<String>) = lua
            .load(format!(
                r#"return fs.resolve_safe("{root_str}", "/etc/passwd")"#
            ))
            .eval()
            .expect("fs.resolve_safe should be callable");

        assert!(resolved.is_none());
        assert!(err.unwrap().contains("Absolute paths not allowed"));
    }

    #[test]
    fn test_resolve_safe_rejects_traversal() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let root_str = dir.path().to_str().unwrap();

        let (resolved, err): (Option<String>, Option<String>) = lua
            .load(format!(
                r#"return fs.resolve_safe("{root_str}", "../../../etc/passwd")"#
            ))
            .eval()
            .expect("fs.resolve_safe should be callable");

        assert!(resolved.is_none());
        assert!(err.unwrap().contains("traversal not allowed"));
    }

    #[test]
    fn test_resolve_safe_rejects_null_byte() {
        let lua = Lua::new();
        register(&lua).expect("Should register fs primitives");

        let dir = tempfile::tempdir().unwrap();
        let root_str = dir.path().to_str().unwrap();

        let (resolved, err): (Option<String>, Option<String>) = lua
            .load(format!(
                r#"return fs.resolve_safe("{root_str}", "test\0.txt")"#
            ))
            .eval()
            .expect("fs.resolve_safe should be callable");

        assert!(resolved.is_none());
        assert!(err.unwrap().contains("null byte"));
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
