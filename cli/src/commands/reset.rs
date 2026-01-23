//! Reset command - removes all botster data from the system.
//!
//! This command provides a clean way to remove all botster-related data:
//! - Notifies server to remove device record (fault-tolerant)
//! - Credentials from OS keyring
//! - Config directory and all files within
//!
//! Useful for troubleshooting, testing fresh installs, or uninstalling.

// Rust guideline compliant 2025-01

use anyhow::Result;
use std::io::{self, Write};

use crate::config::Config;
use crate::device::Device;
use crate::keyring::Credentials;

/// Run the reset command.
///
/// Shows what will be deleted and asks for confirmation (unless `skip_confirm` is true).
pub fn run(skip_confirm: bool) -> Result<()> {
    let config_dir = Config::config_dir().ok();

    println!();
    println!("This will remove all botster data from your system:");
    println!();

    // Show what will be deleted
    println!("  Server:");
    println!("    - Device registration (if reachable)");
    println!();
    println!("  Keyring:");
    println!("    - botster/credentials (API token, MCP token, signing key, Signal keys)");
    println!();

    if let Some(ref dir) = config_dir {
        println!("  Config directory:");
        println!("    - {}", dir.display());

        // List contents if directory exists
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if path.is_dir() {
                        println!("      - {}/", name);
                    } else {
                        println!("      - {}", name);
                    }
                }
            }
        }
    } else {
        println!("  Config directory: (could not determine path)");
    }

    println!();

    // Ask for confirmation
    if !skip_confirm {
        print!("Are you sure you want to delete all this data? [y/N] ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let confirmed = matches!(input.trim().to_lowercase().as_str(), "y" | "yes");
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    println!();
    println!("Removing data...");

    // Notify server before deleting local credentials (best-effort)
    notify_server_of_reset();

    // Delete keyring credentials
    match Credentials::delete() {
        Ok(()) => println!("  ✓ Deleted keyring credentials"),
        Err(e) => println!("  ✗ Failed to delete keyring credentials: {}", e),
    }

    // Delete config directory
    if let Some(ref dir) = config_dir {
        if dir.exists() {
            match std::fs::remove_dir_all(dir) {
                Ok(()) => println!("  ✓ Deleted config directory: {}", dir.display()),
                Err(e) => println!("  ✗ Failed to delete config directory: {}", e),
            }
        } else {
            println!("  - Config directory does not exist (already clean)");
        }
    }

    println!();
    println!("Reset complete. Run 'botster-hub start' to set up fresh.");

    Ok(())
}

/// Notify the server to remove the device record.
///
/// This is best-effort: if it fails (network issues, server down, etc.),
/// we log and continue with local cleanup. The server can clean up stale
/// devices later if needed.
fn notify_server_of_reset() {
    // Load config and device info - if either fails, we can't notify
    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => {
            println!("  - No config found, skipping server notification");
            return;
        }
    };

    if !config.has_token() {
        println!("  - No API token found, skipping server notification");
        return;
    }

    let device = match Device::load_or_create() {
        Ok(d) => d,
        Err(_) => {
            println!("  - No device identity found, skipping server notification");
            return;
        }
    };

    let device_id = match device.device_id {
        Some(id) => id,
        None => {
            println!("  - Device not registered with server, skipping notification");
            return;
        }
    };

    // Send DELETE request to remove device from server
    let url = format!("{}/devices/{}", config.server_url, device_id);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());

    match client.delete(&url).bearer_auth(config.get_api_key()).send() {
        Ok(response) if response.status().is_success() => {
            println!("  ✓ Notified server (device removed)");
        }
        Ok(response) => {
            // Server returned an error, but we continue anyway
            println!(
                "  - Server returned {}, continuing with local cleanup",
                response.status()
            );
        }
        Err(e) => {
            // Network error, server unreachable, etc.
            println!(
                "  - Could not reach server ({}), continuing with local cleanup",
                e
            );
        }
    }
}
