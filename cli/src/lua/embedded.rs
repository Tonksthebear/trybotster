//! Embedded Lua files for release builds.
//!
//! This module includes Lua files that were embedded at compile time by build.rs.
//! In release builds, these are used when no user override files exist.
//!
//! # Build Process
//!
//! The `build.rs` script walks `cli/lua/` and generates `embedded_lua.rs` containing:
//! - `EMBEDDED_LUA_FILES`: Array of (path, content) tuples
//! - `get_embedded_lua(path)`: Lookup function
//!
//! # Usage
//!
//! ```ignore
//! use crate::lua::embedded;
//!
//! // Get a specific file
//! if let Some(content) = embedded::get("hub/init.lua") {
//!     lua.load(content).exec()?;
//! }
//!
//! // Iterate all embedded files
//! for (path, content) in embedded::all() {
//!     println!("Embedded: {}", path);
//! }
//! ```

// Include the generated file from build.rs
#[allow(missing_docs)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/embedded_lua.rs"));
}
use generated::{get_embedded_lua, EMBEDDED_LUA_FILES};

/// Get embedded Lua file content by path.
///
/// Wrapper around the generated `get_embedded_lua` function.
#[inline]
pub fn get(path: &str) -> Option<&'static str> {
    get_embedded_lua(path)
}

/// Get all embedded Lua files as (path, content) pairs.
#[inline]
pub fn all() -> &'static [(&'static str, &'static str)] {
    EMBEDDED_LUA_FILES
}

/// Check if a file is embedded.
#[inline]
pub fn contains(path: &str) -> bool {
    get_embedded_lua(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Debug builds use empty stubs (Lua files loaded from filesystem for hot-reload).
    // Release builds embed all Lua files via include_str!().
    // These tests verify both behaviors are correct.

    #[test]
    fn test_debug_build_has_empty_stubs() {
        if cfg!(debug_assertions) {
            assert!(all().is_empty(), "Debug builds should not embed Lua files");
            assert!(get("hub/init.lua").is_none(), "Debug builds return None for all lookups");
        }
    }

    #[test]
    fn test_release_build_embeds_core_files() {
        if !cfg!(debug_assertions) {
            assert!(contains("hub/init.lua"), "Release build should embed hub/init.lua");
            let content = get("hub/init.lua").unwrap();
            assert!(content.contains("Botster"), "Should contain Botster identifier");

            let files = all();
            assert!(!files.is_empty(), "Release build should have embedded files");

            let paths: Vec<_> = files.iter().map(|(p, _)| *p).collect();
            assert!(paths.contains(&"hub/init.lua"));
            assert!(paths.contains(&"hub/state.lua"));
            assert!(paths.contains(&"hub/hooks.lua"));
            assert!(paths.contains(&"lib/agent.lua"));
            assert!(paths.contains(&"lib/client.lua"));
            assert!(paths.contains(&"handlers/agents.lua"));
            assert!(paths.contains(&"handlers/webrtc.lua"));
        }
    }

    #[test]
    fn test_nonexistent_returns_none() {
        assert!(get("nonexistent.lua").is_none());
        assert!(!contains("also/nonexistent.lua"));
    }

    /// Walks `cli/lua/` on disk (same logic as build.rs) and collects all `.lua` files.
    ///
    /// Returns relative paths like `"lib/agent.lua"`, `"hub/init.lua"`, etc.
    fn collect_lua_files_on_disk() -> Vec<String> {
        use std::path::Path;

        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(base, &path, out);
                    } else if path.extension().map_or(false, |ext| ext == "lua") {
                        let rel = path
                            .strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        out.push(rel);
                    }
                }
            }
        }

        // Resolve cli/lua/ relative to CARGO_MANIFEST_DIR (the cli/ crate root)
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set");
        let lua_dir = Path::new(&manifest).join("lua");
        assert!(lua_dir.exists(), "cli/lua/ directory should exist: {}", lua_dir.display());

        let mut files = Vec::new();
        walk(&lua_dir, &lua_dir, &mut files);
        files.sort();
        files
    }

    /// Extracts all `require("...")` and `safe_require("...")` calls from hub/init.lua.
    ///
    /// Returns module names like `"lib.agent"`, `"handlers.connections"`, etc.
    fn extract_init_requires() -> Vec<String> {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be set");
        let init_path = std::path::Path::new(&manifest).join("lua/hub/init.lua");
        let content = std::fs::read_to_string(&init_path)
            .unwrap_or_else(|e| panic!("Should read hub/init.lua at {}: {}", init_path.display(), e));

        // Simple parser: find require("...") and safe_require("...") patterns
        let mut modules = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            // Skip comments
            if trimmed.starts_with("--") {
                continue;
            }
            // Find require(" and safe_require(" calls
            for needle in &["require(\"", "safe_require(\""] {
                let mut search_from = 0;
                while let Some(start) = trimmed[search_from..].find(needle) {
                    let abs_start = search_from + start + needle.len();
                    if let Some(end) = trimmed[abs_start..].find('"') {
                        modules.push(trimmed[abs_start..abs_start + end].to_string());
                    }
                    search_from = abs_start;
                }
            }
        }
        modules
    }

    /// Verifies every module that hub/init.lua requires has a corresponding
    /// `.lua` file on disk.
    ///
    /// This catches the exact bug where `lib/agent.lua` exists on disk but
    /// isn't embedded: if the file doesn't exist, it can never be embedded.
    /// If it does exist but isn't embedded, the build cache is stale.
    #[test]
    fn test_all_init_requires_have_files_on_disk() {
        let files_on_disk = collect_lua_files_on_disk();
        let requires = extract_init_requires();

        // user.init is an optional user-extensible module (loaded via safe_require)
        let optional_modules = ["user.init"];

        for module in &requires {
            if optional_modules.contains(&module.as_str()) {
                continue;
            }

            // Convert module name to file path: "lib.agent" → "lib/agent.lua"
            let file_path = format!("{}.lua", module.replace('.', "/"));

            // Also check for package-style init: "lib.agent" → "lib/agent/init.lua"
            let init_path = format!("{}/init.lua", module.replace('.', "/"));

            assert!(
                files_on_disk.contains(&file_path) || files_on_disk.contains(&init_path),
                "hub/init.lua requires \"{}\" but neither {} nor {} exists in cli/lua/.\n\
                 Files on disk: {:?}",
                module,
                file_path,
                init_path,
                files_on_disk,
            );
        }
    }

    /// Verifies that build.rs's `collect_lua_files` would find every critical
    /// module file, including `lib/agent.lua`.
    ///
    /// This is the TDD anchor: if `lib/agent.lua` is missing from the
    /// collected files, the embedded searcher will fail at runtime with
    /// `module 'lib.agent' not found`.
    #[test]
    fn test_critical_modules_exist_on_disk() {
        let files = collect_lua_files_on_disk();

        // These modules are required by handlers and MUST be embedded.
        // The original bug: lib/agent.lua was added but build.rs didn't
        // re-run due to missing subdirectory watches, so the release
        // binary shipped without it.
        let critical = [
            "lib/agent.lua",
            "lib/client.lua",
            "lib/commands.lua",
            "lib/config_resolver.lua",
            "handlers/agents.lua",
            "handlers/connections.lua",
            "handlers/webrtc.lua",
            "handlers/tui.lua",
            "handlers/hub_commands.lua",
            "handlers/commands.lua",
            "handlers/filesystem.lua",
            "handlers/templates.lua",
            "hub/init.lua",
            "hub/state.lua",
            "hub/hooks.lua",
            "hub/loader.lua",
        ];

        for path in &critical {
            assert!(
                files.contains(&path.to_string()),
                "Critical module {} not found in cli/lua/. \
                 build.rs must embed this file for release builds to work.\n\
                 Files found: {:?}",
                path,
                files,
            );
        }
    }

    /// Verifies that the release build embeds every `.lua` file from disk.
    ///
    /// In debug mode this is a no-op (embedded is empty by design).
    /// In release mode, this catches stale build caches where build.rs
    /// didn't re-run after new files were added to subdirectories.
    #[test]
    fn test_release_embeds_all_disk_files() {
        if cfg!(debug_assertions) {
            return; // Debug builds intentionally have empty stubs
        }

        let disk_files = collect_lua_files_on_disk();
        let embedded_paths: Vec<&str> = all().iter().map(|(p, _)| *p).collect();

        for disk_file in &disk_files {
            assert!(
                embedded_paths.contains(&disk_file.as_str()),
                "File {} exists on disk but is NOT in the embedded binary. \
                 This likely means build.rs didn't re-run after the file was added. \
                 Run `cargo clean` and rebuild, or fix build.rs subdirectory watches.",
                disk_file,
            );
        }
    }
}
