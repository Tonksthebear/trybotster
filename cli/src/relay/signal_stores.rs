//! Signal Protocol store implementations.
//!
//! This module provides in-memory stores for all Signal Protocol state,
//! with encrypted persistence to disk using AES-256-GCM + OS keyring.
//!
//! # Store Traits Implemented
//!
//! - `IdentityKeyStore` - Our identity + known peer identities
//! - `SessionStore` - Per-peer Double Ratchet sessions
//! - `PreKeyStore` - One-time PreKeys (consumed on use)
//! - `SignedPreKeyStore` - Signed PreKeys (rotated periodically)
//! - `KyberPreKeyStore` - Post-quantum Kyber keys
//! - `SenderKeyStore` - Group messaging keys
//!
//! Rust guideline compliant 2025-01

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use libsignal_protocol::{
    Direction, GenericSignedPreKey, IdentityChange, IdentityKey, IdentityKeyPair, KyberPreKeyId,
    KyberPreKeyRecord, KyberPreKeyStore, PreKeyId, PreKeyRecord, PreKeyStore, ProtocolAddress,
    PublicKey, SenderKeyRecord, SenderKeyStore, SessionRecord, SessionStore, SignedPreKeyId,
    SignedPreKeyRecord, SignedPreKeyStore, SignalProtocolError,
};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::persistence::SignalStoreState;

/// Unified Signal Protocol store with all key material.
///
/// Uses in-memory storage with encrypted disk persistence.
/// Cloneable - clones share the underlying data via Arc.
#[derive(Clone)]
pub struct SignalProtocolStore {
    // Note: IdentityKeyPair doesn't implement Debug, so we can't derive Debug.
    // Use manual Debug impl below.
    /// Our identity key pair.
    identity_key_pair: IdentityKeyPair,
    /// Registration ID (random 14-bit value).
    registration_id: u32,
    /// Known peer identities (address -> identity key).
    identities: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Sessions (address -> session record bytes).
    sessions: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// One-time PreKeys (id -> record bytes).
    pre_keys: Arc<RwLock<HashMap<u32, Vec<u8>>>>,
    /// Signed PreKeys (id -> record bytes).
    signed_pre_keys: Arc<RwLock<HashMap<u32, Vec<u8>>>>,
    /// Kyber PreKeys (id -> record bytes).
    kyber_pre_keys: Arc<RwLock<HashMap<u32, Vec<u8>>>>,
    /// Sender keys for groups ((address, distribution_id) -> record bytes).
    sender_keys: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Track which PreKeys have been used.
    used_pre_keys: Arc<RwLock<Vec<u32>>>,
}

impl std::fmt::Debug for SignalProtocolStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalProtocolStore")
            .field("registration_id", &self.registration_id)
            .finish_non_exhaustive()
    }
}

impl SignalProtocolStore {
    /// Create a new store with fresh identity.
    pub async fn new() -> Result<Self> {
        let identity_key_pair = IdentityKeyPair::generate(&mut rand::rng());
        // Registration ID is a 14-bit random value
        let registration_id = rand::random::<u32>() & 0x3FFF;

        Ok(Self {
            identity_key_pair,
            registration_id,
            identities: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pre_keys: Arc::new(RwLock::new(HashMap::new())),
            signed_pre_keys: Arc::new(RwLock::new(HashMap::new())),
            kyber_pre_keys: Arc::new(RwLock::new(HashMap::new())),
            sender_keys: Arc::new(RwLock::new(HashMap::new())),
            used_pre_keys: Arc::new(RwLock::new(Vec::new())),
        })
    }

    /// Load store from encrypted disk storage.
    pub async fn load(hub_id: &str) -> Result<Self> {
        let state = super::persistence::load_signal_store(hub_id)?;

        log::info!(
            "Loaded Signal store: {} sessions, {} identities, {} prekeys",
            state.sessions.len(),
            state.identities.len(),
            state.pre_keys.len()
        );
        for key in state.sessions.keys() {
            log::debug!("  Restored session: {}", key);
        }

        let identity_key_pair = IdentityKeyPair::try_from(state.identity_key_pair.as_slice())
            .map_err(|e| anyhow::anyhow!("Failed to deserialize identity: {e}"))?;

        Ok(Self {
            identity_key_pair,
            registration_id: state.registration_id,
            identities: Arc::new(RwLock::new(state.identities)),
            sessions: Arc::new(RwLock::new(state.sessions)),
            pre_keys: Arc::new(RwLock::new(state.pre_keys)),
            signed_pre_keys: Arc::new(RwLock::new(state.signed_pre_keys)),
            kyber_pre_keys: Arc::new(RwLock::new(state.kyber_pre_keys)),
            sender_keys: Arc::new(RwLock::new(state.sender_keys)),
            used_pre_keys: Arc::new(RwLock::new(state.used_pre_keys)),
        })
    }

    /// Persist store to encrypted disk storage.
    pub async fn persist(&self, hub_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await.clone();
        let identities = self.identities.read().await.clone();

        log::debug!(
            "Persisting Signal store: {} sessions, {} identities",
            sessions.len(),
            identities.len()
        );
        for key in sessions.keys() {
            log::debug!("  Session: {}", key);
        }

        let state = SignalStoreState {
            identity_key_pair: self.identity_key_pair.serialize().to_vec(),
            registration_id: self.registration_id,
            identities,
            sessions,
            pre_keys: self.pre_keys.read().await.clone(),
            signed_pre_keys: self.signed_pre_keys.read().await.clone(),
            kyber_pre_keys: self.kyber_pre_keys.read().await.clone(),
            sender_keys: self.sender_keys.read().await.clone(),
            used_pre_keys: self.used_pre_keys.read().await.clone(),
        };

        super::persistence::save_signal_store(hub_id, &state)?;
        Ok(())
    }

    /// Get identity key pair.
    pub async fn get_identity_key_pair(&self) -> std::result::Result<IdentityKeyPair, SignalProtocolError> {
        Ok(self.identity_key_pair.clone())
    }

    /// Get local registration ID.
    pub async fn get_local_registration_id(&self) -> std::result::Result<u32, SignalProtocolError> {
        Ok(self.registration_id)
    }

    /// Get an available PreKey ID (any that hasn't been consumed).
    ///
    /// Returns None if all PreKeys have been consumed.
    pub async fn get_available_prekey_id(&self) -> Option<u32> {
        let pre_keys = self.pre_keys.read().await;
        pre_keys.keys().next().copied()
    }

    /// Get count of remaining PreKeys.
    pub async fn prekey_count(&self) -> usize {
        self.pre_keys.read().await.len()
    }

    /// Create address key for HashMap.
    fn address_key(address: &ProtocolAddress) -> String {
        format!("{}:{}", address.name(), address.device_id())
    }

    /// Create sender key map key.
    fn sender_key_key(sender: &ProtocolAddress, distribution_id: Uuid) -> String {
        format!("{}:{}:{}", sender.name(), sender.device_id(), distribution_id)
    }
}

// ============================================================================
// IdentityKeyStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl libsignal_protocol::IdentityKeyStore for SignalProtocolStore {
    async fn get_identity_key_pair(&self) -> std::result::Result<IdentityKeyPair, SignalProtocolError> {
        Ok(self.identity_key_pair.clone())
    }

    async fn get_local_registration_id(&self) -> std::result::Result<u32, SignalProtocolError> {
        Ok(self.registration_id)
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> std::result::Result<IdentityChange, SignalProtocolError> {
        let key = Self::address_key(address);
        let mut identities = self.identities.write().await;

        let existing = identities.get(&key).cloned();
        let new_bytes = identity.serialize().to_vec();

        let change = match existing {
            Some(old) if old != new_bytes => IdentityChange::ReplacedExisting,
            _ => IdentityChange::NewOrUnchanged,
        };
        identities.insert(key, new_bytes);

        Ok(change)
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> std::result::Result<bool, SignalProtocolError> {
        let key = Self::address_key(address);
        let identities = self.identities.read().await;

        match identities.get(&key) {
            Some(known) => {
                // Trust if it matches what we have
                Ok(*known == identity.serialize().to_vec())
            }
            None => {
                // Trust on first use (TOFU)
                Ok(true)
            }
        }
    }

    async fn get_identity(
        &self,
        address: &ProtocolAddress,
    ) -> std::result::Result<Option<IdentityKey>, SignalProtocolError> {
        let key = Self::address_key(address);
        let identities = self.identities.read().await;

        match identities.get(&key) {
            Some(bytes) => {
                let identity = IdentityKey::try_from(bytes.as_slice())
                    .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid identity: {e}")))?;
                Ok(Some(identity))
            }
            None => Ok(None),
        }
    }
}

// ============================================================================
// SessionStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl SessionStore for SignalProtocolStore {
    async fn load_session(
        &self,
        address: &ProtocolAddress,
    ) -> std::result::Result<Option<SessionRecord>, SignalProtocolError> {
        let key = Self::address_key(address);
        let sessions = self.sessions.read().await;

        match sessions.get(&key) {
            Some(bytes) => {
                let record = SessionRecord::deserialize(bytes)
                    .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid session: {e}")))?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
    ) -> std::result::Result<(), SignalProtocolError> {
        let key = Self::address_key(address);
        let bytes = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidArgument(format!("Failed to serialize session: {e}")))?;

        let mut sessions = self.sessions.write().await;
        sessions.insert(key, bytes);
        Ok(())
    }
}

// ============================================================================
// PreKeyStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl PreKeyStore for SignalProtocolStore {
    async fn get_pre_key(
        &self,
        id: PreKeyId,
    ) -> std::result::Result<PreKeyRecord, SignalProtocolError> {
        let pre_keys = self.pre_keys.read().await;
        let id_u32: u32 = id.into();

        match pre_keys.get(&id_u32) {
            Some(bytes) => PreKeyRecord::deserialize(bytes)
                .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid PreKey: {e}"))),
            None => Err(SignalProtocolError::InvalidPreKeyId),
        }
    }

    async fn save_pre_key(
        &mut self,
        id: PreKeyId,
        record: &PreKeyRecord,
    ) -> std::result::Result<(), SignalProtocolError> {
        let id_u32: u32 = id.into();
        let bytes = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidArgument(format!("Failed to serialize PreKey: {e}")))?;

        let mut pre_keys = self.pre_keys.write().await;
        pre_keys.insert(id_u32, bytes);
        Ok(())
    }

    async fn remove_pre_key(&mut self, id: PreKeyId) -> std::result::Result<(), SignalProtocolError> {
        let id_u32: u32 = id.into();

        let mut pre_keys = self.pre_keys.write().await;
        pre_keys.remove(&id_u32);

        // Track that this PreKey was used
        let mut used = self.used_pre_keys.write().await;
        if !used.contains(&id_u32) {
            used.push(id_u32);
        }

        Ok(())
    }
}

// ============================================================================
// SignedPreKeyStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl SignedPreKeyStore for SignalProtocolStore {
    async fn get_signed_pre_key(
        &self,
        id: SignedPreKeyId,
    ) -> std::result::Result<SignedPreKeyRecord, SignalProtocolError> {
        let signed_pre_keys = self.signed_pre_keys.read().await;
        let id_u32: u32 = id.into();

        match signed_pre_keys.get(&id_u32) {
            Some(bytes) => SignedPreKeyRecord::deserialize(bytes)
                .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid SignedPreKey: {e}"))),
            None => Err(SignalProtocolError::InvalidSignedPreKeyId),
        }
    }

    async fn save_signed_pre_key(
        &mut self,
        id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> std::result::Result<(), SignalProtocolError> {
        let id_u32: u32 = id.into();
        let bytes = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidArgument(format!("Failed to serialize SignedPreKey: {e}")))?;

        let mut signed_pre_keys = self.signed_pre_keys.write().await;
        signed_pre_keys.insert(id_u32, bytes);
        Ok(())
    }
}

// ============================================================================
// KyberPreKeyStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl KyberPreKeyStore for SignalProtocolStore {
    async fn get_kyber_pre_key(
        &self,
        id: KyberPreKeyId,
    ) -> std::result::Result<KyberPreKeyRecord, SignalProtocolError> {
        let kyber_pre_keys = self.kyber_pre_keys.read().await;
        let id_u32: u32 = id.into();

        match kyber_pre_keys.get(&id_u32) {
            Some(bytes) => KyberPreKeyRecord::deserialize(bytes)
                .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid KyberPreKey: {e}"))),
            None => Err(SignalProtocolError::InvalidKyberPreKeyId),
        }
    }

    async fn save_kyber_pre_key(
        &mut self,
        id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> std::result::Result<(), SignalProtocolError> {
        let id_u32: u32 = id.into();
        let bytes = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidArgument(format!("Failed to serialize KyberPreKey: {e}")))?;

        let mut kyber_pre_keys = self.kyber_pre_keys.write().await;
        kyber_pre_keys.insert(id_u32, bytes);
        Ok(())
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        _ec_prekey_id: SignedPreKeyId,
        _base_key: &PublicKey,
    ) -> std::result::Result<(), SignalProtocolError> {
        // For now, we don't remove Kyber keys after use (they can be reused as last-resort)
        // In a more complete implementation, you might want to track usage
        log::debug!("KyberPreKey {} marked as used", u32::from(kyber_prekey_id));
        Ok(())
    }
}

// ============================================================================
// SenderKeyStore Implementation
// ============================================================================

#[async_trait(?Send)]
impl SenderKeyStore for SignalProtocolStore {
    async fn store_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: Uuid,
        record: &SenderKeyRecord,
    ) -> std::result::Result<(), SignalProtocolError> {
        let key = Self::sender_key_key(sender, distribution_id);
        let bytes = record.serialize()
            .map_err(|e| SignalProtocolError::InvalidArgument(format!("Failed to serialize SenderKey: {e}")))?;

        let mut sender_keys = self.sender_keys.write().await;
        sender_keys.insert(key, bytes);
        Ok(())
    }

    async fn load_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: Uuid,
    ) -> std::result::Result<Option<SenderKeyRecord>, SignalProtocolError> {
        let key = Self::sender_key_key(sender, distribution_id);
        let sender_keys = self.sender_keys.read().await;

        match sender_keys.get(&key) {
            Some(bytes) => {
                let record = SenderKeyRecord::deserialize(bytes)
                    .map_err(|e| SignalProtocolError::InvalidArgument(format!("Invalid SenderKey: {e}")))?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_creation() {
        let store = SignalProtocolStore::new().await.unwrap();
        let identity = store.get_identity_key_pair().await.unwrap();
        assert!(!identity.public_key().serialize().is_empty());
    }

    #[tokio::test]
    async fn test_registration_id() {
        let store = SignalProtocolStore::new().await.unwrap();
        let reg_id = store.get_local_registration_id().await.unwrap();
        // Should be 14-bit value
        assert!(reg_id < 0x4000);
    }

    #[tokio::test]
    async fn test_prekey_storage() {
        use libsignal_protocol::KeyPair;

        let mut store = SignalProtocolStore::new().await.unwrap();

        let key_pair = KeyPair::generate(&mut rand::rng());
        let record = PreKeyRecord::new(PreKeyId::from(42), &key_pair);

        store.save_pre_key(PreKeyId::from(42), &record).await.unwrap();

        let loaded = store.get_pre_key(PreKeyId::from(42)).await.unwrap();
        assert_eq!(
            loaded.public_key().unwrap().serialize(),
            record.public_key().unwrap().serialize()
        );
    }
}
