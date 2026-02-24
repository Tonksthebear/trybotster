//! Plugin-scoped encrypted secret storage for Lua scripts.
//!
//! Provides namespaced secret storage backed by AES-256-GCM encrypted
//! files on disk. The encryption key is derived from a master key stored
//! in the OS keyring (loaded once, cached in memory). Lua code never
//! touches the keyring directly.
//!
//! # Storage structure
//!
//! ```text
//! ~/.config/botster/secrets/{namespace}/{key}.enc
//! ```
//!
//! Each `.enc` file is a JSON envelope containing a base64 nonce and
//! ciphertext, using the shared `crate::crypto` format.
//!
//! # Security
//!
//! - Lua has zero keyring access — only encrypted file I/O
//! - Namespace/key names validated: alphanumeric, hyphens, underscores
//! - Maximum lengths enforced (namespace: 64, key: 64, value: 8192)
//! - Files written with 0o600 permissions on Unix
//! - Encryption key cached in process memory after first load
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Store a secret
//! local ok, err = secrets.set("github", "mcp_token", "btmcp_abc123")
//!
//! -- Retrieve a secret
//! local val, err = secrets.get("github", "mcp_token")
//!
//! -- Delete a secret
//! local ok, err = secrets.delete("github", "mcp_token")
//! ```

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use mlua::Lua;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use crate::crypto::EncryptedData;
use crate::keyring::Credentials;

/// Secrets format version.
const SECRETS_VERSION: u8 = 1;
/// Maximum namespace name length.
const MAX_NAMESPACE_LEN: usize = 64;
/// Maximum key name length.
const MAX_KEY_LEN: usize = 64;
/// Maximum secret value length.
const MAX_VALUE_LEN: usize = 8192;
/// Keyring field name for the secrets master key.
const SECRETS_CRYPTO_KEY_ID: &str = "__secrets_master__";

/// Cached master encryption key (loaded once from keyring).
fn master_key_cache() -> &'static RwLock<Option<[u8; 32]>> {
    static CACHE: OnceLock<RwLock<Option<[u8; 32]>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

/// Check if we're in test mode.
fn is_test_mode() -> bool {
    #[cfg(test)]
    {
        return true;
    }

    #[cfg(not(test))]
    {
        crate::env::should_skip_keyring()
    }
}

/// Get the secrets base directory.
fn secrets_base_dir() -> Result<PathBuf> {
    #[cfg(test)]
    {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("cli/ has parent directory")
            .join("tmp/botster-test/secrets");
        return Ok(dir);
    }

    #[cfg(not(test))]
    {
        if let Ok(custom_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
            return Ok(PathBuf::from(custom_dir).join("secrets"));
        }

        if crate::env::should_skip_keyring() {
            return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("cli/ has parent directory")
                .join("tmp/botster-test/secrets"));
        }

        dirs::config_dir()
            .context("Could not determine config directory")
            .map(|d| d.join(crate::env::APP_NAME).join("secrets"))
    }
}

/// Get or create the master encryption key for secrets.
///
/// In production: stored in the consolidated keyring entry under
/// `crypto_keys["__secrets_master__"]`. Loaded once, cached in memory.
/// In test mode: deterministic key derived from a fixed seed.
fn get_or_create_master_key() -> Result<[u8; 32]> {
    if is_test_mode() {
        let hash = Sha256::digest(b"test-secrets-master-key");
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash[..32]);
        return Ok(key);
    }

    // Check cache
    {
        let cache = master_key_cache().read().expect("secrets key cache poisoned");
        if let Some(key) = *cache {
            return Ok(key);
        }
    }

    // Load or generate via keyring
    let mut creds = Credentials::load().unwrap_or_default();

    let key = if let Some(key_b64) = creds.crypto_key(SECRETS_CRYPTO_KEY_ID) {
        let key_bytes = BASE64
            .decode(key_b64)
            .context("Invalid secrets master key encoding")?;
        key_bytes
            .try_into()
            .map_err(|_| anyhow!("Invalid secrets master key length"))
    } else {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);

        creds.set_crypto_key(SECRETS_CRYPTO_KEY_ID.to_string(), BASE64.encode(key));
        creds.save()?;

        log::info!("Generated secrets master encryption key");
        Ok(key)
    }?;

    // Cache
    {
        let mut cache = master_key_cache().write().expect("secrets key cache poisoned");
        *cache = Some(key);
    }

    Ok(key)
}

/// Derive a per-namespace key from the master key.
/// This ensures one namespace's encrypted files can't be decrypted
/// by swapping files between namespaces.
fn derive_namespace_key(master: &[u8; 32], namespace: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(master);
    hasher.update(b":namespace:");
    hasher.update(namespace.as_bytes());
    let hash = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&hash[..32]);
    key
}

/// Validate a namespace or key name.
fn validate_name(name: &str, label: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    let max = if label == "namespace" {
        MAX_NAMESPACE_LEN
    } else {
        MAX_KEY_LEN
    };
    if name.len() > max {
        return Err(format!("{label} exceeds maximum length of {max}"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{label} may only contain alphanumeric characters, hyphens, and underscores"
        ));
    }
    Ok(())
}

/// Resolve the file path for a secret.
fn secret_path(namespace: &str, key: &str) -> Result<PathBuf> {
    let base = secrets_base_dir()?;
    Ok(base.join(namespace).join(format!("{key}.enc")))
}

/// Read a secret from encrypted storage.
pub fn read_secret(namespace: &str, key: &str) -> Result<Option<String>> {
    let path = secret_path(namespace, key)?;
    if !path.exists() {
        return Ok(None);
    }

    let master = get_or_create_master_key()?;
    let ns_key = derive_namespace_key(&master, namespace);

    let content = fs::read_to_string(&path).context("Failed to read secret file")?;
    let encrypted: EncryptedData =
        serde_json::from_str(&content).context("Failed to parse secret file")?;

    let plaintext = crate::crypto::decrypt(&ns_key, &encrypted)?;
    let value = String::from_utf8(plaintext).context("Secret is not valid UTF-8")?;

    Ok(Some(value))
}

/// Write a secret to encrypted storage.
pub fn write_secret(namespace: &str, key: &str, value: &str) -> Result<()> {
    let path = secret_path(namespace, key)?;

    // Create parent directories
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create secret directory")?;
    }

    let master = get_or_create_master_key()?;
    let ns_key = derive_namespace_key(&master, namespace);

    let encrypted = crate::crypto::encrypt(&ns_key, value.as_bytes(), SECRETS_VERSION)?;
    let content =
        serde_json::to_string_pretty(&encrypted).context("Failed to serialize encrypted secret")?;

    fs::write(&path, content).context("Failed to write secret file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&path, perms).context("Failed to set secret file permissions")?;
    }

    Ok(())
}

/// Delete a secret from storage. Cleans up empty namespace directory.
pub fn delete_secret(namespace: &str, key: &str) -> Result<()> {
    let path = secret_path(namespace, key)?;
    if path.exists() {
        fs::remove_file(&path).context("Failed to delete secret file")?;
    }

    // Clean up empty namespace directory
    if let Some(parent) = path.parent() {
        if parent.exists() && fs::read_dir(parent)?.next().is_none() {
            let _ = fs::remove_dir(parent);
        }
    }

    Ok(())
}

/// Register the `secrets` table with encrypted secret functions.
///
/// Creates a global `secrets` table with methods:
/// - `secrets.get(namespace, key)` - Read a secret
/// - `secrets.set(namespace, key, value)` - Write a secret
/// - `secrets.delete(namespace, key)` - Delete a secret
pub fn register(lua: &Lua) -> Result<()> {
    let secrets_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create secrets table: {e}"))?;

    // secrets.get(namespace, key) -> (string, nil) or (nil, error_string)
    let get_fn = lua
        .create_function(|_, (namespace, key): (String, String)| {
            if let Err(e) = validate_name(&namespace, "namespace") {
                return Ok((None::<String>, Some(e)));
            }
            if let Err(e) = validate_name(&key, "key") {
                return Ok((None::<String>, Some(e)));
            }

            match read_secret(&namespace, &key) {
                Ok(val) => Ok((val, None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("{e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create secrets.get function: {e}"))?;

    secrets_table
        .set("get", get_fn)
        .map_err(|e| anyhow!("Failed to set secrets.get: {e}"))?;

    // secrets.set(namespace, key, value) -> (true, nil) or (nil, error_string)
    let set_fn = lua
        .create_function(|_, (namespace, key, value): (String, String, String)| {
            if let Err(e) = validate_name(&namespace, "namespace") {
                return Ok((None::<bool>, Some(e)));
            }
            if let Err(e) = validate_name(&key, "key") {
                return Ok((None::<bool>, Some(e)));
            }
            if value.len() > MAX_VALUE_LEN {
                return Ok((
                    None::<bool>,
                    Some(format!("value exceeds maximum length of {MAX_VALUE_LEN}")),
                ));
            }

            match write_secret(&namespace, &key, &value) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("{e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create secrets.set function: {e}"))?;

    secrets_table
        .set("set", set_fn)
        .map_err(|e| anyhow!("Failed to set secrets.set: {e}"))?;

    // secrets.delete(namespace, key) -> (true, nil) or (nil, error_string)
    let delete_fn = lua
        .create_function(|_, (namespace, key): (String, String)| {
            if let Err(e) = validate_name(&namespace, "namespace") {
                return Ok((None::<bool>, Some(e)));
            }
            if let Err(e) = validate_name(&key, "key") {
                return Ok((None::<bool>, Some(e)));
            }

            match delete_secret(&namespace, &key) {
                Ok(()) => Ok((Some(true), None::<String>)),
                Err(e) => Ok((None::<bool>, Some(format!("{e}")))),
            }
        })
        .map_err(|e| anyhow!("Failed to create secrets.delete function: {e}"))?;

    secrets_table
        .set("delete", delete_fn)
        .map_err(|e| anyhow!("Failed to set secrets.delete: {e}"))?;

    lua.globals()
        .set("secrets", secrets_table)
        .map_err(|e| anyhow!("Failed to register secrets table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Function, Table};

    #[test]
    fn test_secrets_table_created() {
        let lua = Lua::new();
        register(&lua).expect("Should register secrets primitives");

        let globals = lua.globals();
        let secrets_table: Table = globals.get("secrets").expect("secrets table should exist");

        let _: Function = secrets_table.get("get").expect("secrets.get should exist");
        let _: Function = secrets_table.get("set").expect("secrets.set should exist");
        let _: Function = secrets_table
            .get("delete")
            .expect("secrets.delete should exist");
    }

    #[test]
    fn test_validate_name_valid() {
        assert!(validate_name("github", "namespace").is_ok());
        assert!(validate_name("my-plugin", "namespace").is_ok());
        assert!(validate_name("my_plugin_2", "key").is_ok());
    }

    #[test]
    fn test_validate_name_rejects_invalid() {
        assert!(validate_name("", "namespace").is_err());
        assert!(validate_name("my.plugin", "namespace").is_err());
        assert!(validate_name("my/plugin", "namespace").is_err());
        assert!(validate_name("../escape", "namespace").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAMESPACE_LEN + 1), "namespace").is_err());
    }

    #[test]
    fn test_namespace_key_derivation_differs() {
        let master = [0u8; 32];
        let key_a = derive_namespace_key(&master, "github");
        let key_b = derive_namespace_key(&master, "slack");
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn test_secret_roundtrip() {
        let ns = "test-roundtrip";
        let key = "my_token";
        let value = "btmcp_test_secret_value";

        // Write
        write_secret(ns, key, value).unwrap();

        // Read
        let loaded = read_secret(ns, key).unwrap();
        assert_eq!(loaded, Some(value.to_string()));

        // Delete
        delete_secret(ns, key).unwrap();
        let after_delete = read_secret(ns, key).unwrap();
        assert_eq!(after_delete, None);
    }

    #[test]
    fn test_read_nonexistent_returns_none() {
        let result = read_secret("nonexistent-ns", "nonexistent-key").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_cross_namespace_isolation() {
        write_secret("ns-a", "token", "secret_a").unwrap();
        write_secret("ns-b", "token", "secret_b").unwrap();

        assert_eq!(read_secret("ns-a", "token").unwrap(), Some("secret_a".to_string()));
        assert_eq!(read_secret("ns-b", "token").unwrap(), Some("secret_b".to_string()));

        // Cleanup
        delete_secret("ns-a", "token").unwrap();
        delete_secret("ns-b", "token").unwrap();
    }
}
