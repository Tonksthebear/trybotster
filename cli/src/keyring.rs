//! Consolidated keyring storage for all CLI credentials.
//!
//! Stores all secrets in a single keyring entry to avoid multiple
//! macOS keychain prompts when the binary changes (new builds).
//!
//! # Storage Hierarchy
//!
//! 1. OS keyring (macOS Keychain, GNOME Keyring, KDE Wallet) — encrypted, preferred
//! 2. File fallback `{config_dir}/credentials.json` — `0600` permissions, used when
//!    keyring is unavailable (headless Linux, WSL2, etc.)
//! 3. Test mode always uses the file fallback.
//!
//! # Graceful Degradation
//!
//! When the OS keyring is unavailable (e.g., headless server without GNOME Keyring),
//! the user is prompted once to confirm file-based storage. On subsequent runs,
//! credentials are loaded from whichever backend has them.
//!
//! macOS keychain may block access when binary signature changes (new builds).
//! This module implements retry logic and distinguishes between:
//! - Keyring locked (user can unlock)
//! - Entry missing (normal first-run)
//! - Access denied (signature mismatch, may need re-auth)

use anyhow::Result;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Keyring service name.
const KEYRING_SERVICE: &str = "botster";
/// Consolidated keyring entry name.
const KEYRING_CREDENTIALS: &str = "credentials";

/// Number of retry attempts for keyring access.
const KEYRING_RETRY_ATTEMPTS: u32 = 2;
/// Delay between retry attempts in milliseconds.
const KEYRING_RETRY_DELAY_MS: u64 = 500;

/// Categorized keyring access errors for better user feedback.
#[derive(Debug)]
pub enum KeyringAccessError {
    /// Keyring is locked and requires user interaction to unlock.
    Locked(String),
    /// Entry does not exist (normal for first run).
    NotFound,
    /// Access denied, likely due to binary signature change.
    AccessDenied(String),
    /// Data exists but is corrupted or unparseable.
    Corrupted(String),
    /// Other/unknown error.
    Other(String),
}

impl std::fmt::Display for KeyringAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked(msg) => write!(f, "Keyring locked: {msg}"),
            Self::NotFound => write!(f, "Keyring entry not found"),
            Self::AccessDenied(msg) => write!(f, "Keyring access denied: {msg}"),
            Self::Corrupted(msg) => write!(f, "Keyring data corrupted: {msg}"),
            Self::Other(msg) => write!(f, "Keyring error: {msg}"),
        }
    }
}

impl std::error::Error for KeyringAccessError {}

/// Categorize a keyring error for better user feedback.
fn categorize_keyring_error(err: &keyring::Error) -> KeyringAccessError {
    let msg = format!("{err:?}");
    let msg_lower = msg.to_lowercase();

    // Check for common macOS keychain error patterns
    if msg_lower.contains("no password")
        || msg_lower.contains("not found")
        || msg_lower.contains("nopassword")
    {
        return KeyringAccessError::NotFound;
    }

    if msg_lower.contains("user interaction") || msg_lower.contains("user canceled") {
        return KeyringAccessError::Locked(msg);
    }

    if msg_lower.contains("denied")
        || msg_lower.contains("codesign")
        || msg_lower.contains("authorization")
        || msg_lower.contains("not allowed")
    {
        return KeyringAccessError::AccessDenied(msg);
    }

    KeyringAccessError::Other(msg)
}

/// Check if keyring should be skipped (any test mode).
///
/// Uses multiple checks to ensure keyring is never accessed during tests:
/// 1. `#[cfg(test)]` - Always skip during Rust unit tests
/// 2. Direct env var check - Fallback if env module detection fails
/// 3. `crate::env::should_skip_keyring()` - Standard environment detection
fn should_skip_keyring() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        // Direct env var check as a safety fallback.
        // This catches cases where BOTSTER_ENV is set but env detection
        // might fail or be called before module initialization.
        if let Ok(env_val) = std::env::var("BOTSTER_ENV") {
            if env_val == "test" || env_val == "system_test" {
                return true;
            }
        }

        // Standard environment detection
        crate::env::should_skip_keyring()
    }
}

/// Get the credentials file path (used for file-based fallback and test mode).
fn credentials_file_path() -> Result<PathBuf> {
    crate::config::Config::config_dir().map(|d| d.join("credentials.json"))
}

/// Prompt the user about keyring unavailability and ask to continue with file storage.
///
/// On non-interactive sessions (no TTY), automatically falls back without prompting.
fn prompt_keyring_fallback() -> Result<()> {
    use std::io::{self, Write};

    let path = credentials_file_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/.config/botster/credentials.json".to_string());

    eprintln!();
    eprintln!("  System keyring is not available on this device.");
    eprintln!();
    eprintln!("  To enable encrypted credential storage, install a keyring service:");
    eprintln!("    Ubuntu/Debian:  sudo apt install gnome-keyring libsecret-1-0 dbus-x11");
    eprintln!("    Then start it:  eval $(dbus-launch --sh-syntax) && \\");
    eprintln!("                    echo \"\" | gnome-keyring-daemon --unlock --start --components=secrets");
    eprintln!();
    eprintln!("  Note: WSL2 keyring support is unreliable and not recommended.");
    eprintln!();
    eprintln!("  Without a keyring, credentials will be stored in:");
    eprintln!("    {} (protected by file permissions 0600 only)", path);
    eprintln!();

    // Non-interactive: fall back automatically
    if !atty::is(atty::Stream::Stdin) {
        eprintln!("  Non-interactive session detected, using file storage.");
        eprintln!();
        return Ok(());
    }

    print!("  Continue without keyring? [Y/n] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "n" || input == "no" {
        anyhow::bail!(
            "Keyring required. Install a keyring service and try again, \
             or set BOTSTER_TOKEN env var to skip keyring entirely."
        );
    }

    Ok(())
}

/// Consolidated credentials stored in a single keyring entry.
///
/// This reduces macOS keychain prompts to 1 on new binary builds.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    /// API token for hub-server authentication (btstr_...).
    /// Used by the hub process for full server access.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,

    /// MCP token for agent authentication (btmcp_...).
    /// Scoped to MCP operations only, passed to spawned agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_token: Option<String>,

    /// Base64-encoded Ed25519 signing key for device identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,

    /// Device fingerprint (used to identify which signing key this is).
    /// Stored alongside signing_key for verification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,

    /// Per-hub crypto encryption keys (hub_id -> base64 AES key).
    /// Used to encrypt Matrix crypto session state at rest.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub crypto_keys: HashMap<String, String>,

    /// Schema version for future migrations.
    #[serde(default = "default_version")]
    pub version: u8,
}

fn default_version() -> u8 {
    1
}

impl Credentials {
    /// Load credentials from the best available backend.
    ///
    /// Tries in order: test file (if test mode), OS keyring, file fallback.
    /// On macOS, keychain access may fail temporarily when:
    /// - Keychain is locked and awaiting user interaction
    /// - Binary signature changed (new build)
    pub fn load() -> Result<Self> {
        if should_skip_keyring() {
            return Self::load_from_file();
        }

        // Try OS keyring first
        let keyring_result = Self::load_from_keyring_with_retry();
        if let Ok(ref creds) = keyring_result {
            if creds.api_token.is_some() || creds.signing_key.is_some() {
                return keyring_result;
            }
        }

        // Keyring returned empty or failed — check file fallback
        match Self::load_from_file() {
            Ok(file_creds)
                if file_creds.api_token.is_some() || file_creds.signing_key.is_some() =>
            {
                log::debug!("Loaded credentials from file fallback");
                Ok(file_creds)
            }
            _ => keyring_result,
        }
    }

    /// Load from keyring with retry logic for transient failures.
    ///
    /// Always succeeds by returning empty credentials on failure.
    /// Uses `Result` for API consistency with `load()`.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Result type for API consistency with load()"
    )]
    fn load_from_keyring_with_retry() -> Result<Self> {
        let mut last_error: Option<KeyringAccessError> = None;

        for attempt in 0..KEYRING_RETRY_ATTEMPTS {
            if attempt > 0 {
                log::debug!(
                    "Retrying keyring access (attempt {}/{})",
                    attempt + 1,
                    KEYRING_RETRY_ATTEMPTS
                );
                thread::sleep(Duration::from_millis(KEYRING_RETRY_DELAY_MS));
            }

            match Self::try_load_from_keyring() {
                Ok(creds) => return Ok(creds),
                Err(err) => {
                    log::debug!("Keyring access attempt {} failed: {}", attempt + 1, err);

                    // Don't retry for NotFound - that's expected on first run
                    if matches!(err, KeyringAccessError::NotFound) {
                        log::debug!("No credentials found in keyring, returning empty");
                        return Ok(Credentials::default());
                    }

                    // Don't retry for corrupted data - it won't fix itself
                    if matches!(err, KeyringAccessError::Corrupted(_)) {
                        log::warn!(
                            "Keyring data corrupted, returning empty credentials: {}",
                            err
                        );
                        return Ok(Credentials::default());
                    }

                    last_error = Some(err);
                }
            }
        }

        // All retries exhausted - log warning and return empty
        // This allows the app to continue and potentially re-authenticate
        if let Some(err) = &last_error {
            log::warn!(
                "Keyring access failed after {} attempts: {}. \
                 Credentials may need to be re-entered.",
                KEYRING_RETRY_ATTEMPTS,
                err
            );

            // For access denied, provide a helpful hint
            if matches!(err, KeyringAccessError::AccessDenied(_)) {
                log::info!(
                    "Hint: Binary signature may have changed. \
                     You may need to re-authenticate or unlock your keychain."
                );
            }
        }

        Ok(Credentials::default())
    }

    /// Attempt a single load from keyring, categorizing any errors.
    fn try_load_from_keyring() -> std::result::Result<Self, KeyringAccessError> {
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS)
            .map_err(|e| KeyringAccessError::Other(format!("Failed to create entry: {e:?}")))?;

        match entry.get_password() {
            Ok(json) => {
                let creds: Credentials = serde_json::from_str(&json)
                    .map_err(|e| KeyringAccessError::Corrupted(format!("JSON parse error: {e}")))?;
                log::debug!("Loaded consolidated credentials from keyring");
                Ok(creds)
            }
            Err(e) => Err(categorize_keyring_error(&e)),
        }
    }

    /// Load credentials from file (fallback when keyring is unavailable, or test mode).
    fn load_from_file() -> Result<Self> {
        let path = credentials_file_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let creds: Credentials = serde_json::from_str(&content)?;
            log::debug!("Loaded credentials from file: {}", path.display());
            Ok(creds)
        } else {
            log::debug!("No credentials file found at {}", path.display());
            Ok(Credentials::default())
        }
    }

    /// Save credentials to the best available backend.
    ///
    /// Tries OS keyring first. If keyring is unavailable, prompts the user
    /// once and falls back to file-based storage.
    pub fn save(&self) -> Result<()> {
        if should_skip_keyring() {
            return self.save_to_file();
        }

        // Try OS keyring
        let keyring_err = match Self::try_save_to_keyring(self) {
            Ok(()) => {
                log::info!("Saved consolidated credentials to OS keyring");
                return Ok(());
            }
            Err(e) => e,
        };

        // Keyring failed — prompt user for file fallback
        log::warn!("Keyring save failed: {keyring_err}");
        prompt_keyring_fallback()?;
        self.save_to_file()
    }

    /// Attempt to save credentials to the OS keyring.
    fn try_save_to_keyring(&self) -> Result<()> {
        let entry = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS)
            .map_err(|e| anyhow::anyhow!("Failed to create keyring entry: {e:?}"))?;

        let json = serde_json::to_string(self)?;
        entry
            .set_password(&json)
            .map_err(|e| anyhow::anyhow!("Failed to store credentials in keyring: {e:?}"))?;

        Ok(())
    }

    /// Save credentials to file (fallback when keyring is unavailable, or test mode).
    fn save_to_file(&self) -> Result<()> {
        let path = credentials_file_path()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, &json)?;

        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

        log::info!("Saved credentials to file: {}", path.display());
        Ok(())
    }

    /// Delete all credentials from both keyring and file fallback.
    pub fn delete() -> Result<()> {
        // Always try to clean up the file fallback
        if let Ok(path) = credentials_file_path() {
            if path.exists() {
                fs::remove_file(&path)?;
                log::info!("Deleted credentials file: {}", path.display());
            }
        }

        if should_skip_keyring() {
            return Ok(());
        }

        // Also try to clean up the keyring entry
        if let Ok(entry) = Entry::new(KEYRING_SERVICE, KEYRING_CREDENTIALS) {
            let _ = entry.delete_credential();
            log::info!("Deleted credentials from OS keyring");
        }

        Ok(())
    }

    // === Convenience accessors ===

    /// Get API token if set.
    pub fn api_token(&self) -> Option<&str> {
        self.api_token.as_deref()
    }

    /// Set API token.
    pub fn set_api_token(&mut self, token: String) {
        self.api_token = Some(token);
    }

    /// Clear API token.
    pub fn clear_api_token(&mut self) {
        self.api_token = None;
    }

    // === MCP token accessors ===

    /// Get MCP token if set.
    pub fn mcp_token(&self) -> Option<&str> {
        self.mcp_token.as_deref()
    }

    /// Set MCP token.
    pub fn set_mcp_token(&mut self, token: String) {
        self.mcp_token = Some(token);
    }

    /// Clear MCP token.
    pub fn clear_mcp_token(&mut self) {
        self.mcp_token = None;
    }

    /// Get signing key if set.
    pub fn signing_key(&self) -> Option<&str> {
        self.signing_key.as_deref()
    }

    /// Set signing key with fingerprint.
    pub fn set_signing_key(&mut self, key: String, fingerprint: String) {
        self.signing_key = Some(key);
        self.fingerprint = Some(fingerprint);
    }

    /// Check if signing key matches expected fingerprint.
    pub fn signing_key_matches_fingerprint(&self, expected: &str) -> bool {
        self.fingerprint.as_deref() == Some(expected)
    }

    /// Update the fingerprint without changing the signing key.
    ///
    /// Used when the stored fingerprint is stale (e.g., after binary rebuild)
    /// but the signing key is still valid.
    pub fn update_fingerprint(&mut self, fingerprint: String) {
        self.fingerprint = Some(fingerprint);
    }

    // === Crypto key accessors (Matrix crypto) ===

    /// Get crypto encryption key for a hub (Matrix crypto state at rest).
    pub fn crypto_key(&self, hub_id: &str) -> Option<&str> {
        self.crypto_keys.get(hub_id).map(String::as_str)
    }

    /// Set crypto encryption key for a hub.
    pub fn set_crypto_key(&mut self, hub_id: String, key: String) {
        self.crypto_keys.insert(hub_id, key);
    }

    /// Remove crypto encryption key for a hub.
    pub fn remove_crypto_key(&mut self, hub_id: &str) {
        self.crypto_keys.remove(hub_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_roundtrip() {
        let mut creds = Credentials::default();
        creds.api_token = Some("btstr_test123".to_string());
        creds.signing_key = Some("base64key".to_string());
        creds.fingerprint = Some("aa:bb:cc:dd".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        let loaded: Credentials = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.api_token, creds.api_token);
        assert_eq!(loaded.signing_key, creds.signing_key);
        assert_eq!(loaded.fingerprint, creds.fingerprint);
    }

    #[test]
    fn test_credentials_skips_none_fields() {
        let creds = Credentials {
            api_token: Some("token".to_string()),
            mcp_token: None,
            signing_key: None,
            fingerprint: None,
            crypto_keys: HashMap::new(),
            version: 1,
        };

        let json = serde_json::to_string(&creds).unwrap();
        assert!(!json.contains("mcp_token"));
        assert!(!json.contains("signing_key"));
        assert!(!json.contains("fingerprint"));
        assert!(!json.contains("crypto_keys"));
    }

    #[test]
    fn test_crypto_keys_stored_and_retrieved() {
        let mut creds = Credentials::default();
        creds.set_crypto_key("hub123".to_string(), "base64key".to_string());

        assert_eq!(creds.crypto_key("hub123"), Some("base64key"));
        assert_eq!(creds.crypto_key("other"), None);

        creds.remove_crypto_key("hub123");
        assert_eq!(creds.crypto_key("hub123"), None);
    }

    // === MCP Token Tests ===

    #[test]
    fn test_mcp_token_stored_and_retrieved() {
        let mut creds = Credentials::default();
        assert_eq!(creds.mcp_token(), None);

        creds.set_mcp_token("btmcp_test123".to_string());
        assert_eq!(creds.mcp_token(), Some("btmcp_test123"));
    }

    #[test]
    fn test_mcp_token_cleared() {
        let mut creds = Credentials::default();
        creds.set_mcp_token("btmcp_test123".to_string());
        assert!(creds.mcp_token().is_some());

        creds.clear_mcp_token();
        assert_eq!(creds.mcp_token(), None);
    }

    #[test]
    fn test_mcp_token_serialized_in_json() {
        let mut creds = Credentials::default();
        creds.set_mcp_token("btmcp_test123".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        assert!(json.contains("mcp_token"));
        assert!(json.contains("btmcp_test123"));

        let loaded: Credentials = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mcp_token(), Some("btmcp_test123"));
    }

    #[test]
    fn test_mcp_token_skipped_when_none() {
        let creds = Credentials::default();
        let json = serde_json::to_string(&creds).unwrap();
        assert!(!json.contains("mcp_token"));
    }

    #[test]
    fn test_credentials_roundtrip_with_mcp_token() {
        let mut creds = Credentials::default();
        creds.api_token = Some("btstr_hub".to_string());
        creds.set_mcp_token("btmcp_agent".to_string());

        let json = serde_json::to_string(&creds).unwrap();
        let loaded: Credentials = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.api_token(), Some("btstr_hub"));
        assert_eq!(loaded.mcp_token(), Some("btmcp_agent"));
    }

    // === Fingerprint Update Tests ===

    #[test]
    fn test_update_fingerprint_preserves_signing_key() {
        let mut creds = Credentials::default();
        creds.set_signing_key("secret_key".to_string(), "old:fp".to_string());

        // Verify initial state
        assert_eq!(creds.signing_key(), Some("secret_key"));
        assert!(creds.signing_key_matches_fingerprint("old:fp"));

        // Update fingerprint
        creds.update_fingerprint("new:fp".to_string());

        // Key should be preserved, fingerprint updated
        assert_eq!(creds.signing_key(), Some("secret_key"));
        assert!(creds.signing_key_matches_fingerprint("new:fp"));
        assert!(!creds.signing_key_matches_fingerprint("old:fp"));
    }

    // === KeyringAccessError Tests ===

    #[test]
    fn test_keyring_access_error_display() {
        let locked = KeyringAccessError::Locked("user canceled".to_string());
        assert!(locked.to_string().contains("Keyring locked"));

        let not_found = KeyringAccessError::NotFound;
        assert!(not_found.to_string().contains("not found"));

        let denied = KeyringAccessError::AccessDenied("codesign".to_string());
        assert!(denied.to_string().contains("access denied"));

        let corrupted = KeyringAccessError::Corrupted("invalid json".to_string());
        assert!(corrupted.to_string().contains("corrupted"));
    }
}
