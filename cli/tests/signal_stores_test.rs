//! Signal Protocol Store Tests
//!
//! Tests for the Signal Protocol store implementations:
//! - Identity key persistence
//! - PreKey generation and consumption
//! - Session store persistence
//! - Store reload after restart simulation

// Rust guideline compliant 2025-01

use botster_hub::relay::{PreKeyBundleData, SignalProtocolManager, SIGNAL_PROTOCOL_VERSION};
use std::env;
use tempfile::TempDir;

/// Helper to set up a clean test environment.
fn setup_test_env(test_name: &str) -> (TempDir, String) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let hub_id = format!("test-hub-{}-{}", test_name, uuid::Uuid::new_v4());

    // Set the data directory to temp dir
    env::set_var("BOTSTER_DATA_DIR", temp_dir.path());

    (temp_dir, hub_id)
}

/// Test that a new SignalProtocolManager generates valid PreKeyBundle.
#[tokio::test]
async fn test_new_manager_generates_prekey_bundle() {
    let (_temp_dir, hub_id) = setup_test_env("bundle");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle");

    // Verify bundle has correct version
    assert_eq!(bundle.version, SIGNAL_PROTOCOL_VERSION);

    // Verify bundle has hub_id
    assert_eq!(bundle.hub_id, hub_id);

    // Verify bundle has identity key (base64 encoded)
    assert!(!bundle.identity_key.is_empty());

    // Verify bundle has signed prekey
    assert!(!bundle.signed_prekey.is_empty());
    assert!(!bundle.signed_prekey_signature.is_empty());

    // Verify bundle has kyber prekey (post-quantum)
    assert!(!bundle.kyber_prekey.is_empty());
    assert!(!bundle.kyber_prekey_signature.is_empty());

    // Verify bundle has a one-time prekey
    assert!(bundle.prekey_id.is_some());
    assert!(bundle.prekey.is_some());
}

/// Test that manager persists and can be reloaded with same identity.
#[tokio::test]
async fn test_manager_persistence_same_identity() {
    let (_temp_dir, hub_id) = setup_test_env("persist");

    // Create first manager and get identity
    let original_identity = {
        let mut manager = SignalProtocolManager::new(&hub_id)
            .await
            .expect("Failed to create manager");

        let bundle = manager
            .build_prekey_bundle_data(1)
            .await
            .expect("Failed to build bundle");

        bundle.identity_key.clone()
    };

    // Load manager again (simulating restart)
    let loaded_identity = {
        let mut manager = SignalProtocolManager::load_or_create(&hub_id)
            .await
            .expect("Failed to load manager");

        let bundle = manager
            .build_prekey_bundle_data(1)
            .await
            .expect("Failed to build bundle");

        bundle.identity_key.clone()
    };

    // Identity should be the same after reload
    assert_eq!(
        original_identity, loaded_identity,
        "Identity key should persist across restarts"
    );
}

/// Test that PreKeys are consumed and manager finds next available.
#[tokio::test]
async fn test_prekey_consumption_finds_next() {
    let (_temp_dir, hub_id) = setup_test_env("prekey");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    // Get first bundle with PreKey 1
    let bundle1 = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle 1");
    let prekey_id1 = bundle1.prekey_id.expect("Should have prekey");

    // Ask for PreKey 1 again - should still work (not consumed yet)
    let bundle2 = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle 2");
    let prekey_id2 = bundle2.prekey_id.expect("Should have prekey");

    // Same PreKey should be returned since not consumed
    assert_eq!(prekey_id1, prekey_id2);

    // If we ask for a non-existent PreKey, it should find any available
    let bundle3 = manager
        .build_prekey_bundle_data(999)
        .await
        .expect("Failed to build bundle 3");

    // Should still have a valid PreKey
    assert!(bundle3.prekey_id.is_some());
}

/// Test that registration ID is stable across sessions.
#[tokio::test]
async fn test_registration_id_stable() {
    let (_temp_dir, hub_id) = setup_test_env("regid");

    // Create first manager
    let original_reg_id = {
        let mut manager = SignalProtocolManager::new(&hub_id)
            .await
            .expect("Failed to create manager");

        let bundle = manager
            .build_prekey_bundle_data(1)
            .await
            .expect("Failed to build bundle");

        bundle.registration_id
    };

    // Reload and check registration ID
    let loaded_reg_id = {
        let mut manager = SignalProtocolManager::load_or_create(&hub_id)
            .await
            .expect("Failed to load manager");

        let bundle = manager
            .build_prekey_bundle_data(1)
            .await
            .expect("Failed to build bundle");

        bundle.registration_id
    };

    assert_eq!(
        original_reg_id, loaded_reg_id,
        "Registration ID should be stable across restarts"
    );
}

/// Test that device ID is always 1 (CLI device).
#[tokio::test]
async fn test_device_id_is_one() {
    let (_temp_dir, hub_id) = setup_test_env("devid");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle");

    assert_eq!(bundle.device_id, 1, "CLI device ID should always be 1");
}

/// Test PreKeyBundle serialization for QR code.
#[tokio::test]
async fn test_prekey_bundle_serialization() {
    let (_temp_dir, hub_id) = setup_test_env("serial");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle");

    // Serialize to JSON (as would be done for QR code)
    let json = serde_json::to_string(&bundle).expect("Failed to serialize bundle");

    // Deserialize back
    let parsed: PreKeyBundleData = serde_json::from_str(&json).expect("Failed to parse bundle");

    assert_eq!(parsed.version, bundle.version);
    assert_eq!(parsed.hub_id, bundle.hub_id);
    assert_eq!(parsed.identity_key, bundle.identity_key);
}

/// Test that multiple managers for different hubs have different identities.
#[tokio::test]
async fn test_different_hubs_different_identities() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    env::set_var("BOTSTER_DATA_DIR", temp_dir.path());

    let hub_id1 = format!("hub-1-{}", uuid::Uuid::new_v4());
    let hub_id2 = format!("hub-2-{}", uuid::Uuid::new_v4());

    let mut manager1 = SignalProtocolManager::new(&hub_id1)
        .await
        .expect("Failed to create manager 1");
    let mut manager2 = SignalProtocolManager::new(&hub_id2)
        .await
        .expect("Failed to create manager 2");

    let bundle1 = manager1
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle 1");
    let bundle2 = manager2
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle 2");

    assert_ne!(
        bundle1.identity_key, bundle2.identity_key,
        "Different hubs should have different identity keys"
    );
    assert_ne!(
        bundle1.registration_id, bundle2.registration_id,
        "Different hubs should have different registration IDs"
    );
}

/// Test that SignedPreKey has valid signature.
#[tokio::test]
async fn test_signed_prekey_has_signature() {
    let (_temp_dir, hub_id) = setup_test_env("sig");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle");

    // Verify signed prekey fields are populated
    assert!(bundle.signed_prekey_id > 0);
    assert!(!bundle.signed_prekey.is_empty());
    assert!(!bundle.signed_prekey_signature.is_empty());

    // Signature should be base64 encoded
    use base64::{engine::general_purpose::STANDARD, Engine};
    let sig_bytes = STANDARD
        .decode(&bundle.signed_prekey_signature)
        .expect("Signature should be valid base64");

    // Ed25519 signatures are 64 bytes
    assert_eq!(sig_bytes.len(), 64, "Signature should be 64 bytes");
}

/// Test that KyberPreKey (post-quantum) is present.
#[tokio::test]
async fn test_kyber_prekey_present() {
    let (_temp_dir, hub_id) = setup_test_env("kyber");

    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle");

    // Kyber fields must be populated
    assert!(bundle.kyber_prekey_id > 0);
    assert!(!bundle.kyber_prekey.is_empty());
    assert!(!bundle.kyber_prekey_signature.is_empty());

    // Kyber1024 public key is 1568 bytes + 1 type byte = 1569 bytes serialized
    use base64::{engine::general_purpose::STANDARD, Engine};
    let kyber_bytes = STANDARD
        .decode(&bundle.kyber_prekey)
        .expect("Kyber key should be valid base64");

    // libsignal serializes with a type prefix byte
    assert!(
        kyber_bytes.len() >= 1568,
        "Kyber1024 public key should be at least 1568 bytes, got {}",
        kyber_bytes.len()
    );
}

/// Test concurrent manager creation doesn't corrupt state.
#[tokio::test]
async fn test_concurrent_manager_access() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    env::set_var("BOTSTER_DATA_DIR", temp_dir.path());

    let hub_id = format!("hub-concurrent-{}", uuid::Uuid::new_v4());
    let hub_id_clone = hub_id.clone();

    // Create manager in main task
    let mut manager = SignalProtocolManager::new(&hub_id)
        .await
        .expect("Failed to create manager");

    let bundle1 = manager
        .build_prekey_bundle_data(1)
        .await
        .expect("Failed to build bundle 1");

    // Simulate another "process" loading the same hub
    let bundle2 = {
        let mut loaded = SignalProtocolManager::load_or_create(&hub_id_clone)
            .await
            .expect("Failed to load manager");
        loaded
            .build_prekey_bundle_data(1)
            .await
            .expect("Failed to build bundle 2")
    };

    // Both should have same identity
    assert_eq!(bundle1.identity_key, bundle2.identity_key);
}
