//! Crypto Service - Thread-safe wrapper for `VodozemacCrypto`.
//!
//! Since `VodozemacCrypto` is fully synchronous (no async, no `!Send` futures),
//! we can use a simple `Arc<Mutex<VodozemacCrypto>>` instead of the previous
//! message-passing architecture with a dedicated thread.

use std::sync::{Arc, Mutex};

use super::olm_crypto::VodozemacCrypto;

/// Thread-safe crypto service: `Arc<Mutex<VodozemacCrypto>>`.
///
/// Cloneable and shareable across threads. Lock the mutex to perform
/// crypto operations synchronously.
pub type CryptoService = Arc<Mutex<VodozemacCrypto>>;

/// Create a new crypto service for the given hub.
///
/// The CLI's long-term identity persists across reboot; live sessions remain
/// ephemeral and in-memory only.
pub fn create_crypto_service(hub_id: &str) -> CryptoService {
    let crypto = match super::persistence::load_vodozemac_account(hub_id) {
        Ok(Some(account)) => VodozemacCrypto::from_account_pickle(hub_id, account),
        Ok(None) => VodozemacCrypto::new(hub_id),
        Err(e) => {
            log::warn!(
                "Failed to load persisted vodozemac account for hub {}: {e}; creating new identity",
                &hub_id[..hub_id.len().min(8)]
            );
            VodozemacCrypto::new(hub_id)
        }
    };
    log::info!(
        "Created crypto service for hub {}",
        &hub_id[..hub_id.len().min(8)]
    );
    Arc::new(Mutex::new(crypto))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_service_creation() {
        let cs = create_crypto_service("test-crypto-svc");
        let guard = cs.lock().expect("mutex poisoned");
        assert!(!guard.identity_key().is_empty());
    }

    #[test]
    fn test_crypto_service_is_clone() {
        let cs = create_crypto_service("test-crypto-clone");
        let cs2 = Arc::clone(&cs);

        let id1 = cs
            .lock()
            .expect("mutex poisoned")
            .identity_key()
            .to_string();
        let id2 = cs2
            .lock()
            .expect("mutex poisoned")
            .identity_key()
            .to_string();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_crypto_service_device_key_bundle() {
        let cs = create_crypto_service("test-crypto-bundle");
        let mut guard = cs.lock().expect("mutex poisoned");
        let bundle = guard.build_device_key_bundle().unwrap();
        assert_eq!(bundle.version, 6);
        assert!(!bundle.curve25519_key.is_empty());
    }

    #[test]
    fn test_crypto_service_persists_long_term_identity() {
        let hub_id = "test-crypto-persist";

        let cs1 = create_crypto_service(hub_id);
        let id1 = {
            let mut guard = cs1.lock().expect("mutex poisoned");
            let id = guard.identity_key().to_string();
            guard.build_device_key_bundle().unwrap();
            id
        };

        let cs2 = create_crypto_service(hub_id);
        let id2 = cs2
            .lock()
            .expect("mutex poisoned")
            .identity_key()
            .to_string();

        assert_eq!(id1, id2);
    }
}
