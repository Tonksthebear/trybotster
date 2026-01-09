//! Embedded Tailscale binary management.
//!
//! This module handles extracting the embedded Tailscale binary to a usable
//! location on disk. The binary is embedded at compile time and extracted
//! on first use to `~/.botster_hub/bin/tailscale`.
//!
//! # Architecture
//!
//! The Tailscale binary is embedded using `include_bytes!()` which stores
//! the entire binary in the executable. On first run:
//!
//! 1. Check if `~/.botster_hub/bin/tailscale` exists and has correct version
//! 2. If not, extract the embedded binary to that location
//! 3. Return the path to the extracted binary
//!
//! This ensures zero-friction installation - users just need the botster-hub
//! binary and everything else is self-contained.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Version of the embedded Tailscale binary.
/// This must match the version downloaded in build.rs.
pub const EMBEDDED_TAILSCALE_VERSION: &str = "1.76.6";

/// The embedded Tailscale binary, included at compile time.
const TAILSCALE_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tailscale"));

/// Marker file to track which version is extracted.
const VERSION_MARKER: &str = ".tailscale_version";

/// Get the path to the Tailscale binary, extracting it if necessary.
///
/// This function:
/// 1. Checks if the binary is already extracted and up-to-date
/// 2. If not, extracts the embedded binary to `~/.botster_hub/bin/tailscale`
/// 3. Returns the path to the binary
///
/// # Errors
///
/// Returns an error if:
/// - The home directory cannot be determined
/// - The binary directory cannot be created
/// - The binary cannot be written to disk
/// - Permissions cannot be set (Unix only)
pub fn get_tailscale_binary_path() -> Result<PathBuf> {
    let bin_dir = get_bin_directory()?;
    let binary_path = bin_dir.join("tailscale");
    let version_path = bin_dir.join(VERSION_MARKER);

    // Check if we need to extract
    let needs_extraction = if binary_path.exists() && version_path.exists() {
        // Check version
        let existing_version = fs::read_to_string(&version_path).unwrap_or_default();
        existing_version.trim() != EMBEDDED_TAILSCALE_VERSION
    } else {
        true
    };

    if needs_extraction {
        extract_tailscale_binary(&binary_path, &version_path)?;
    }

    Ok(binary_path)
}

/// Get the directory where we store extracted binaries.
fn get_bin_directory() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let bin_dir = home.join(".botster_hub").join("bin");
    fs::create_dir_all(&bin_dir).context("Failed to create bin directory")?;
    Ok(bin_dir)
}

/// Extract the embedded Tailscale binary to disk.
fn extract_tailscale_binary(binary_path: &PathBuf, version_path: &PathBuf) -> Result<()> {
    log::info!(
        "Extracting embedded Tailscale {} to {}",
        EMBEDDED_TAILSCALE_VERSION,
        binary_path.display()
    );

    // Write the binary
    let mut file = File::create(binary_path).context("Failed to create Tailscale binary file")?;
    file.write_all(TAILSCALE_BINARY)
        .context("Failed to write Tailscale binary")?;
    file.flush()?;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(binary_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(binary_path, perms).context("Failed to set executable permissions")?;
    }

    // Write version marker
    fs::write(version_path, EMBEDDED_TAILSCALE_VERSION)
        .context("Failed to write version marker")?;

    log::info!("Tailscale binary extracted successfully");
    Ok(())
}

/// Check if the embedded Tailscale binary appears valid.
///
/// This is a simple sanity check - it verifies the binary is not empty
/// and has a reasonable size (> 1MB for a real binary).
pub fn is_binary_valid() -> bool {
    // A real Tailscale binary is ~30-40MB
    // The placeholder from build.rs is < 1KB
    TAILSCALE_BINARY.len() > 1_000_000
}

/// Get information about the embedded binary.
pub fn get_binary_info() -> BinaryInfo {
    BinaryInfo {
        version: EMBEDDED_TAILSCALE_VERSION.to_string(),
        size_bytes: TAILSCALE_BINARY.len(),
        is_valid: is_binary_valid(),
    }
}

/// Information about the embedded Tailscale binary.
#[derive(Debug)]
pub struct BinaryInfo {
    /// Version of the embedded binary.
    pub version: String,
    /// Size of the embedded binary in bytes.
    pub size_bytes: usize,
    /// Whether the binary appears to be valid (not a placeholder).
    pub is_valid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_info() {
        let info = get_binary_info();
        assert_eq!(info.version, EMBEDDED_TAILSCALE_VERSION);
        // In tests, the binary might be a placeholder
        assert!(info.size_bytes > 0);
    }

    #[test]
    fn test_get_bin_directory() {
        let dir = get_bin_directory();
        assert!(dir.is_ok());
        let dir = dir.expect("get_bin_directory failed");
        assert!(dir.ends_with(".botster_hub/bin"));
    }
}
