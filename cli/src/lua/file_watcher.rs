//! Lua-specific file watcher for hot-reload.
//!
//! Thin wrapper around [`crate::file_watcher::FileWatcher`] that filters
//! for `.lua` files and converts paths to Lua module names. The generic
//! watcher handles all OS-level concerns; this module adds only the
//! Lua-specific transformations.
//!
//! # Usage
//!
//! ```ignore
//! let mut watcher = LuaFileWatcher::new(PathBuf::from("~/.botster/lua"))?;
//! watcher.start_watching()?;
//!
//! // In event loop:
//! for module_name in watcher.poll_changes() {
//!     lua.call_function("loader.reload", module_name)?;
//! }
//! ```

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::file_watcher::{FileEventKind, FileWatcher};

/// Watches a directory for `.lua` changes and yields module names.
///
/// Delegates to [`FileWatcher`] for OS-level watching, then filters
/// for `.lua` files and converts paths to dot-notation module names
/// (e.g., `core/handlers/foo.lua` becomes `core.handlers.foo`).
pub struct LuaFileWatcher {
    /// Generic file watcher handling OS-level events.
    watcher: FileWatcher,
    /// Base path for stripping prefixes and computing module names.
    base_path: PathBuf,
}

impl std::fmt::Debug for LuaFileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LuaFileWatcher")
            .field("base_path", &self.base_path)
            .finish_non_exhaustive()
    }
}

impl LuaFileWatcher {
    /// Create a watcher for Lua files under `base_path`.
    ///
    /// Does not start watching until [`start_watching`](Self::start_watching).
    ///
    /// # Errors
    ///
    /// Returns an error if the OS file watcher cannot be initialized.
    pub fn new(base_path: PathBuf) -> Result<Self> {
        Ok(Self {
            watcher: FileWatcher::new()?,
            base_path,
        })
    }

    /// Start watching the directory recursively.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory does not exist or watch
    /// registration fails.
    pub fn start_watching(&mut self) -> Result<()> {
        self.watcher.watch(&self.base_path, true)?;
        log::info!("Watching for Lua file changes: {:?}", self.base_path);
        Ok(())
    }

    /// Stop watching the directory.
    ///
    /// Safe to call even if not currently watching.
    pub fn stop_watching(&mut self) {
        self.watcher.unwatch(&self.base_path);
    }

    /// Poll for changed Lua modules (non-blocking).
    ///
    /// Returns deduplicated module names in dot notation
    /// (e.g., `core.handlers.foo`). Only `.lua` file creates and
    /// modifications are included; deletes are ignored.
    #[must_use]
    pub fn poll_changes(&self) -> Vec<String> {
        let mut changes = Vec::new();

        for event in self.watcher.poll() {
            if !matches!(event.kind, FileEventKind::Create | FileEventKind::Modify | FileEventKind::Rename) {
                continue;
            }

            if event.path.extension().is_some_and(|ext| ext == "lua") {
                if let Some(module_name) = self.path_to_module(&event.path) {
                    if !changes.contains(&module_name) {
                        changes.push(module_name);
                    }
                }
            }
        }

        changes
    }

    /// Convert a file path to a Lua module name.
    ///
    /// Strips `base_path`, removes the `.lua` extension, and replaces
    /// path separators with dots.
    ///
    /// Returns `None` if the path is outside `base_path`.
    fn path_to_module(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.base_path).ok()?;
        let without_ext = relative.with_extension("");

        let module_name = without_ext
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(".");

        Some(module_name)
    }

    /// Get the base path being watched.
    #[must_use]
    pub fn base_path(&self) -> &PathBuf {
        &self.base_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a `LuaFileWatcher` for testing `path_to_module`.
    fn test_watcher(base_path: &str) -> LuaFileWatcher {
        LuaFileWatcher {
            watcher: FileWatcher::new().expect("Should create watcher"),
            base_path: PathBuf::from(base_path),
        }
    }

    #[test]
    fn test_path_to_module_simple() {
        let fw = test_watcher("/home/user/.botster/lua");
        let path = PathBuf::from("/home/user/.botster/lua/core/init.lua");
        assert_eq!(fw.path_to_module(&path), Some("core.init".to_string()));
    }

    #[test]
    fn test_path_to_module_nested() {
        let fw = test_watcher("/lua");
        let path = PathBuf::from("/lua/handlers/webrtc/message.lua");
        assert_eq!(
            fw.path_to_module(&path),
            Some("handlers.webrtc.message".to_string())
        );
    }

    #[test]
    fn test_path_to_module_root() {
        let fw = test_watcher("/lua");
        let path = PathBuf::from("/lua/init.lua");
        assert_eq!(fw.path_to_module(&path), Some("init".to_string()));
    }

    #[test]
    fn test_path_to_module_outside_base() {
        let fw = test_watcher("/home/user/.botster/lua");
        let path = PathBuf::from("/other/path/module.lua");
        assert_eq!(fw.path_to_module(&path), None);
    }

    #[test]
    fn test_debug_impl() {
        let fw = test_watcher("/test");
        let debug = format!("{fw:?}");
        assert!(debug.contains("LuaFileWatcher"));
        assert!(debug.contains("/test"));
    }
}
