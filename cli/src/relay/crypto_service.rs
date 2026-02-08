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
/// Loads existing state from disk if available, otherwise creates a fresh identity.
pub fn create_crypto_service(hub_id: &str) -> CryptoService {
    let crypto = VodozemacCrypto::load_or_create(hub_id);
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

        let id1 = cs.lock().expect("mutex poisoned").identity_key().to_string();
        let id2 = cs2.lock().expect("mutex poisoned").identity_key().to_string();
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
}
