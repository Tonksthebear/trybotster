//! File watcher for Lua hot-reload.
//!
//! Watches the Lua script directory for changes and reports which modules
//! need to be reloaded. Uses the `notify` crate for cross-platform file
//! system monitoring.
//!
//! # Architecture
//!
//! The watcher runs in non-blocking mode - the Hub's event loop polls for
//! changes periodically. When a `.lua` file is modified, the watcher converts
//! the file path to a Lua module name (e.g., `core/handlers/foo.lua` becomes
//! `core.handlers.foo`).
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

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// File watcher for Lua hot-reload.
///
/// Watches a directory for changes to `.lua` files and converts file paths
/// to Lua module names for reloading.
pub struct LuaFileWatcher {
    /// The underlying file system watcher.
    watcher: RecommendedWatcher,
    /// Channel receiver for file system events.
    rx: mpsc::Receiver<Result<Event, notify::Error>>,
    /// Base path being watched (for converting paths to module names).
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
    /// Create a new file watcher for the given directory.
    ///
    /// Does not start watching until [`start_watching`](Self::start_watching) is called.
    ///
    /// # Arguments
    ///
    /// * `base_path` - Directory to watch for Lua file changes
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be created (e.g., system limits).
    pub fn new(base_path: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::channel();

        let watcher = notify::recommended_watcher(move |res| {
            // Send events to the channel; ignore send errors (receiver might be dropped)
            let _ = tx.send(res);
        })
        .context("Failed to create file watcher")?;

        Ok(Self {
            watcher,
            rx,
            base_path,
        })
    }

    /// Start watching the directory for changes.
    ///
    /// Watches recursively, so subdirectories like `core/handlers/` are included.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory does not exist
    /// - Watch registration fails (e.g., too many watches)
    pub fn start_watching(&mut self) -> Result<()> {
        self.watcher
            .watch(&self.base_path, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch directory: {:?}", self.base_path))?;

        log::info!("Watching for Lua file changes: {:?}", self.base_path);
        Ok(())
    }

    /// Stop watching the directory.
    ///
    /// Safe to call even if not currently watching.
    pub fn stop_watching(&mut self) {
        let _ = self.watcher.unwatch(&self.base_path);
    }

    /// Poll for file changes (non-blocking).
    ///
    /// Returns a list of Lua module names that have changed and should be
    /// reloaded. Module names use dot notation (e.g., `core.handlers.foo`).
    ///
    /// Changes are deduplicated - if the same file changed multiple times
    /// between polls, it appears only once in the result.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let changes = watcher.poll_changes();
    /// for module_name in changes {
    ///     // Call loader.reload(module_name) in Lua
    /// }
    /// ```
    #[must_use]
    pub fn poll_changes(&self) -> Vec<String> {
        let mut changes = Vec::new();

        // Drain all available events
        while let Ok(result) = self.rx.try_recv() {
            if let Ok(event) = result {
                // Only care about modifications and creations
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    for path in event.paths {
                        // Only process .lua files
                        if path.extension().is_some_and(|ext| ext == "lua") {
                            if let Some(module_name) = self.path_to_module(&path) {
                                // Deduplicate
                                if !changes.contains(&module_name) {
                                    changes.push(module_name);
                                }
                            }
                        }
                    }
                }
            }
        }

        changes
    }

    /// Convert a file path to a Lua module name.
    ///
    /// - Strips the base path prefix
    /// - Removes the `.lua` extension
    /// - Replaces path separators with dots
    ///
    /// # Examples
    ///
    /// - `~/.botster/lua/core/handlers/foo.lua` -> `core.handlers.foo`
    /// - `~/.botster/lua/init.lua` -> `init`
    fn path_to_module(&self, path: &PathBuf) -> Option<String> {
        // Strip base path
        let relative = path.strip_prefix(&self.base_path).ok()?;

        // Remove .lua extension
        let without_ext = relative.with_extension("");

        // Convert path separators to dots
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

    #[test]
    fn test_path_to_module_simple() {
        // Create watcher with test base path (don't need to actually watch)
        let (tx, _rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let _ = tx.send(res);
        })
        .unwrap();

        let file_watcher = LuaFileWatcher {
            watcher,
            rx: mpsc::channel().1,
            base_path: PathBuf::from("/home/user/.botster/lua"),
        };

        let path = PathBuf::from("/home/user/.botster/lua/core/init.lua");
        let module = file_watcher.path_to_module(&path);
        assert_eq!(module, Some("core.init".to_string()));
    }

    #[test]
    fn test_path_to_module_nested() {
        let (tx, _rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let _ = tx.send(res);
        })
        .unwrap();

        let file_watcher = LuaFileWatcher {
            watcher,
            rx: mpsc::channel().1,
            base_path: PathBuf::from("/lua"),
        };

        let path = PathBuf::from("/lua/handlers/webrtc/message.lua");
        let module = file_watcher.path_to_module(&path);
        assert_eq!(module, Some("handlers.webrtc.message".to_string()));
    }

    #[test]
    fn test_path_to_module_root() {
        let (tx, _rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let _ = tx.send(res);
        })
        .unwrap();

        let file_watcher = LuaFileWatcher {
            watcher,
            rx: mpsc::channel().1,
            base_path: PathBuf::from("/lua"),
        };

        let path = PathBuf::from("/lua/init.lua");
        let module = file_watcher.path_to_module(&path);
        assert_eq!(module, Some("init".to_string()));
    }

    #[test]
    fn test_path_to_module_outside_base() {
        let (tx, _rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let _ = tx.send(res);
        })
        .unwrap();

        let file_watcher = LuaFileWatcher {
            watcher,
            rx: mpsc::channel().1,
            base_path: PathBuf::from("/home/user/.botster/lua"),
        };

        // Path outside base should return None
        let path = PathBuf::from("/other/path/module.lua");
        let module = file_watcher.path_to_module(&path);
        assert_eq!(module, None);
    }
}
