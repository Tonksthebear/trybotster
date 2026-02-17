//! Cross-platform file system event monitoring.
//!
//! Provides a generic [`FileWatcher`] backed by OS-native mechanisms
//! (kqueue on macOS, inotify on Linux) via the `notify` crate. Supports
//! two consumption modes:
//!
//! - **Polling**: Call [`FileWatcher::poll`] to drain buffered events.
//! - **Event-driven**: Call [`FileWatcher::take_rx`] to extract the raw
//!   receiver, then spawn a blocking forwarder task that sends events
//!   to the Hub event channel.
//!
//! This module is the foundation for both Lua hot-reload
//! ([`crate::lua::file_watcher::LuaFileWatcher`]) and the Lua `watch`
//! primitives.

use std::path::Path;
use std::sync::mpsc;

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// Classification of a file system event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEventKind {
    /// A new file or directory was created.
    Create,
    /// File content or metadata was modified (not a rename).
    Modify,
    /// A file or directory was renamed or moved.
    Rename,
    /// A file or directory was deleted.
    Delete,
    /// Event type not mapped to a specific category.
    ///
    /// Includes access events, watcher-internal events, etc.
    /// Consumers that only care about mutations can skip these.
    Other,
}

/// A single file system event with path and classification.
#[derive(Debug, Clone)]
pub struct FileEvent {
    /// Absolute path of the affected file or directory.
    pub path: std::path::PathBuf,
    /// What happened to the file.
    pub kind: FileEventKind,
}

/// Non-blocking file system watcher using OS-native mechanisms.
///
/// Wraps `notify::RecommendedWatcher` with a channel-based event interface.
/// Events can be consumed either by polling via [`poll`](Self::poll) or by
/// extracting the receiver via [`take_rx`](Self::take_rx) for event-driven
/// delivery through a blocking forwarder task.
pub struct FileWatcher {
    watcher: RecommendedWatcher,
    rx: Option<mpsc::Receiver<Result<Event, notify::Error>>>,
}

impl std::fmt::Debug for FileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatcher").finish_non_exhaustive()
    }
}

impl FileWatcher {
    /// Create a new watcher with no active watches.
    ///
    /// Call [`watch`](Self::watch) to start monitoring paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the OS file watcher cannot be initialized
    /// (e.g., system resource limits).
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();

        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .context("Failed to create file watcher")?;

        Ok(Self { watcher, rx: Some(rx) })
    }

    /// Start watching `path` for file system events.
    ///
    /// When `recursive` is true, all subdirectories are included.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not exist or watch
    /// registration fails (e.g., too many watches on Linux).
    pub fn watch(&mut self, path: &Path, recursive: bool) -> Result<()> {
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        self.watcher
            .watch(path, mode)
            .with_context(|| format!("Failed to watch: {}", path.display()))?;

        log::info!("Watching for file changes: {:?}", path);
        Ok(())
    }

    /// Stop watching `path`.
    ///
    /// Safe to call even if `path` is not currently watched.
    pub fn unwatch(&mut self, path: &Path) {
        let _ = self.watcher.unwatch(path);
    }

    /// Extract the raw receiver for event-driven delivery.
    ///
    /// After calling this, [`poll`](Self::poll) will return empty results.
    /// The caller owns the receiver and should drain it in a blocking
    /// forwarder task that sends events to the Hub event channel.
    ///
    /// Returns `None` if the receiver was already taken.
    pub fn take_rx(&mut self) -> Option<mpsc::Receiver<Result<Event, notify::Error>>> {
        self.rx.take()
    }

    /// Drain all buffered events (non-blocking).
    ///
    /// Returns every event that arrived since the last call. Returns
    /// an empty `Vec` if nothing changed or if the receiver was taken
    /// via [`take_rx`](Self::take_rx). Errors from the underlying
    /// watcher are logged and skipped.
    #[must_use]
    pub fn poll(&self) -> Vec<FileEvent> {
        let Some(ref rx) = self.rx else {
            return Vec::new();
        };

        let mut events = Vec::new();

        while let Ok(result) = rx.try_recv() {
            match result {
                Ok(event) => {
                    let kind = Self::classify(&event.kind);
                    for path in event.paths {
                        events.push(FileEvent { path, kind });
                    }
                }
                Err(e) => {
                    log::warn!("File watcher error: {e}");
                }
            }
        }

        events
    }

    /// Classify a raw `notify::Event` into [`FileEvent`] items.
    ///
    /// Utility for event-driven consumers that receive raw events from
    /// the extracted receiver (via [`take_rx`](Self::take_rx)).
    pub fn classify_event(event: &notify::Event) -> Vec<FileEvent> {
        let kind = Self::classify(&event.kind);
        event.paths.iter().map(|p| FileEvent { path: p.clone(), kind }).collect()
    }

    /// Map `notify::EventKind` to [`FileEventKind`].
    ///
    /// Renames are distinguished from other modifications so consumers
    /// can react to file moves without re-parsing notify internals.
    fn classify(kind: &notify::EventKind) -> FileEventKind {
        match kind {
            notify::EventKind::Create(_) => FileEventKind::Create,
            notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => FileEventKind::Rename,
            notify::EventKind::Modify(_) => FileEventKind::Modify,
            notify::EventKind::Remove(_) => FileEventKind::Delete,
            _ => FileEventKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_create_watcher() {
        let watcher = FileWatcher::new();
        assert!(watcher.is_ok());
    }

    #[test]
    fn test_watch_nonexistent_path_fails() {
        let mut watcher = FileWatcher::new().expect("Should create watcher");
        let result = watcher.watch(Path::new("/nonexistent/path/abc123"), false);
        assert!(result.is_err());
    }

    #[test]
    fn test_poll_empty_initially() {
        let watcher = FileWatcher::new().expect("Should create watcher");
        let events = watcher.poll();
        assert!(events.is_empty());
    }

    #[test]
    fn test_unwatch_nonexistent_is_safe() {
        let mut watcher = FileWatcher::new().expect("Should create watcher");
        // Should not panic
        watcher.unwatch(Path::new("/some/path"));
    }

    #[test]
    fn test_classify_create() {
        let kind = notify::EventKind::Create(notify::event::CreateKind::File);
        assert_eq!(FileWatcher::classify(&kind), FileEventKind::Create);
    }

    #[test]
    fn test_classify_modify() {
        let kind = notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        ));
        assert_eq!(FileWatcher::classify(&kind), FileEventKind::Modify);
    }

    #[test]
    fn test_classify_rename() {
        let kind = notify::EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::Both,
        ));
        assert_eq!(FileWatcher::classify(&kind), FileEventKind::Rename);
    }

    #[test]
    fn test_classify_remove() {
        let kind = notify::EventKind::Remove(notify::event::RemoveKind::File);
        assert_eq!(FileWatcher::classify(&kind), FileEventKind::Delete);
    }

    #[test]
    fn test_classify_access_is_other() {
        let kind = notify::EventKind::Access(notify::event::AccessKind::Read);
        assert_eq!(FileWatcher::classify(&kind), FileEventKind::Other);
    }

    #[test]
    fn test_watch_real_directory() {
        let dir = std::env::temp_dir().join("botster_fw_test");
        let _ = std::fs::create_dir_all(&dir);

        let mut watcher = FileWatcher::new().expect("Should create watcher");
        let result = watcher.watch(&dir, false);
        assert!(result.is_ok());

        watcher.unwatch(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_debug_impl() {
        let watcher = FileWatcher::new().expect("Should create watcher");
        let debug = format!("{watcher:?}");
        assert!(debug.contains("FileWatcher"));
    }

    #[test]
    fn test_file_event_kind_eq() {
        assert_eq!(FileEventKind::Create, FileEventKind::Create);
        assert_ne!(FileEventKind::Create, FileEventKind::Modify);
        assert_ne!(FileEventKind::Modify, FileEventKind::Delete);
    }

    #[test]
    fn test_file_event_debug() {
        let event = FileEvent {
            path: PathBuf::from("/test/file.txt"),
            kind: FileEventKind::Create,
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("Create"));
        assert!(debug.contains("file.txt"));
    }
}
