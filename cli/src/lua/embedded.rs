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
//! if let Some(content) = embedded::get("core/init.lua") {
//!     lua.load(content).exec()?;
//! }
//!
//! // Iterate all embedded files
//! for (path, content) in embedded::all() {
//!     println!("Embedded: {}", path);
//! }
//! ```

// Include the generated file from build.rs
include!(concat!(env!("OUT_DIR"), "/embedded_lua.rs"));

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
            assert!(get("core/init.lua").is_none(), "Debug builds return None for all lookups");
        }
    }

    #[test]
    fn test_release_build_embeds_core_files() {
        if !cfg!(debug_assertions) {
            assert!(contains("core/init.lua"), "Release build should embed core/init.lua");
            let content = get("core/init.lua").unwrap();
            assert!(content.contains("Botster"), "Should contain Botster identifier");

            let files = all();
            assert!(!files.is_empty(), "Release build should have embedded files");

            let paths: Vec<_> = files.iter().map(|(p, _)| *p).collect();
            assert!(paths.contains(&"core/init.lua"));
            assert!(paths.contains(&"core/state.lua"));
            assert!(paths.contains(&"core/hooks.lua"));
            assert!(paths.contains(&"lib/client.lua"));
            assert!(paths.contains(&"handlers/webrtc.lua"));
        }
    }

    #[test]
    fn test_nonexistent_returns_none() {
        assert!(get("nonexistent.lua").is_none());
        assert!(!contains("also/nonexistent.lua"));
    }
}
