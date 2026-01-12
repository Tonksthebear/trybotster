//! Signal Protocol E2E Encryption.
//!
//! This module provides E2E encryption using libsignal's implementation of the
//! Signal Protocol, including X3DH key agreement and Double Ratchet messaging.
//!
//! # Protocol Flow
//!
//! ```text
//! CLI (Server)                              Browser (Client)
//! ──────────────────────────────────────────────────────────
//! 1. Generate IdentityKeyPair
//! 2. Generate PreKeys + SignedPreKey + KyberPreKey
//! 3. Display QR code with PreKeyBundle
//!
//!                                   4. Scan QR, get PreKeyBundle
//!                                   5. process_prekey_bundle()
//!                                   6. Send PreKeySignalMessage ──►
//!
//! 7. Receive PreKeySignalMessage
//! 8. message_decrypt_prekey() creates session
//! 9. Both sides now have Double Ratchet session
//!
//!    ◄── Encrypted messages (SignalMessage) ──►
//! ```
//!
//! # Group Messaging (SenderKey)
//!
//! For CLI → multiple browsers broadcast:
//! 1. CLI creates SenderKeyDistributionMessage
//! 2. CLI sends distribution to each browser via individual session
//! 3. CLI uses group_encrypt() for broadcasts
//! 4. Browsers use group_decrypt() to receive
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use libsignal_protocol::{
    // Core types
    KeyPair, ProtocolAddress, DeviceId,
    // Messages
    CiphertextMessageType, PreKeySignalMessage, SignalMessage,
    // Session and keys
    GenericSignedPreKey, PreKeyRecord, SignedPreKeyRecord, KyberPreKeyRecord,
    PreKeyId, SignedPreKeyId, KyberPreKeyId, Timestamp,
    // Operations
    message_encrypt, message_decrypt_prekey, message_decrypt_signal,
    group_encrypt, create_sender_key_distribution_message,
    // Stores
    SessionStore, PreKeyStore, SignedPreKeyStore, KyberPreKeyStore,
};
use serde::{Deserialize, Serialize};
use rand::SeedableRng;
use sha2::{Sha256, Digest};
use std::time::SystemTime;

use super::signal_stores::SignalProtocolStore;

/// Protocol version for Signal messages.
pub const SIGNAL_PROTOCOL_VERSION: u8 = 4;

/// CLI device ID (always 1 - the "server" device).
pub const CLI_DEVICE_ID: u32 = 1;

/// Keys needed for session establishment, included in QR code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreKeyBundleData {
    /// Protocol version.
    pub version: u8,
    /// Hub identifier for routing.
    pub hub_id: String,
    /// Registration ID for this device.
    pub registration_id: u32,
    /// Device ID (CLI = 1).
    pub device_id: u32,
    /// Identity public key (base64).
    pub identity_key: String,
    /// Signed PreKey ID.
    pub signed_prekey_id: u32,
    /// Signed PreKey public key (base64).
    pub signed_prekey: String,
    /// Signed PreKey signature (base64).
    pub signed_prekey_signature: String,
    /// One-time PreKey ID (optional).
    pub prekey_id: Option<u32>,
    /// One-time PreKey public key (base64, optional).
    pub prekey: Option<String>,
    /// Kyber PreKey ID.
    pub kyber_prekey_id: u32,
    /// Kyber PreKey public key (base64).
    pub kyber_prekey: String,
    /// Kyber PreKey signature (base64).
    pub kyber_prekey_signature: String,
}

/// Encrypted Signal message envelope (protocol v4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEnvelope {
    /// Protocol version (4 for Signal).
    pub version: u8,
    /// Message type:
    /// - 1 = PreKeySignalMessage (initial, contains PreKey info)
    /// - 2 = SignalMessage (regular Double Ratchet message)
    /// - 3 = SenderKeyMessage (group broadcast)
    pub message_type: u8,
    /// Base64-encoded ciphertext.
    pub ciphertext: String,
    /// Sender's identity public key (base64).
    pub sender_identity: String,
    /// Sender's registration ID.
    pub registration_id: u32,
    /// Sender's device ID.
    pub device_id: u32,
}

impl SignalEnvelope {
    /// Message type for PreKeySignalMessage.
    pub const MSG_TYPE_PREKEY: u8 = 1;
    /// Message type for SignalMessage.
    pub const MSG_TYPE_SIGNAL: u8 = 2;
    /// Message type for SenderKeyMessage (group).
    pub const MSG_TYPE_SENDER_KEY: u8 = 3;
}

/// Signal Protocol manager for CLI-side encryption.
///
/// Manages identity, sessions, and key material for secure communication
/// with browser clients.
pub struct SignalProtocolManager {
    /// Protocol store containing all key material.
    store: SignalProtocolStore,
    /// Hub identifier for persistence.
    hub_id: String,
    /// Our protocol address.
    our_address: ProtocolAddress,
    /// Group ID for SenderKey broadcasts (derived from hub_id).
    group_id: uuid::Uuid,
}

impl std::fmt::Debug for SignalProtocolManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalProtocolManager")
            .field("hub_id", &self.hub_id)
            .field("our_address", &self.our_address)
            .finish_non_exhaustive()
    }
}

impl SignalProtocolManager {
    /// Create a new Signal Protocol manager with fresh identity.
    ///
    /// Generates new identity keys, PreKeys, SignedPreKey, and KyberPreKey.
    pub async fn new(hub_id: &str) -> Result<Self> {
        let store = SignalProtocolStore::new().await?;

        // Derive a stable address from hub_id
        let address_name = Self::derive_address_name(hub_id);
        let our_address = ProtocolAddress::new(
            address_name,
            DeviceId::new(CLI_DEVICE_ID as u8).expect("valid device ID"),
        );

        // Derive group ID from hub_id for SenderKey
        let group_id = Self::derive_group_id(hub_id);

        let mut manager = Self {
            store,
            hub_id: hub_id.to_string(),
            our_address,
            group_id,
        };

        // Generate initial key material
        manager.generate_prekeys().await?;

        // Persist the new keys so they survive CLI restarts
        manager.store.persist(hub_id).await?;

        log::info!(
            "Created new SignalProtocolManager for hub {}",
            &hub_id[..hub_id.len().min(8)]
        );

        Ok(manager)
    }

    /// Load existing manager or create new one.
    pub async fn load_or_create(hub_id: &str) -> Result<Self> {
        // Try to load existing store
        match SignalProtocolStore::load(hub_id).await {
            Ok(store) => {
                let address_name = Self::derive_address_name(hub_id);
                let our_address = ProtocolAddress::new(
            address_name,
            DeviceId::new(CLI_DEVICE_ID as u8).expect("valid device ID"),
        );
                let group_id = Self::derive_group_id(hub_id);

                log::info!(
                    "Loaded existing SignalProtocolManager for hub {}",
                    &hub_id[..hub_id.len().min(8)]
                );

                Ok(Self {
                    store,
                    hub_id: hub_id.to_string(),
                    our_address,
                    group_id,
                })
            }
            Err(_) => Self::new(hub_id).await,
        }
    }

    /// Generate PreKeys for session establishment.
    async fn generate_prekeys(&mut self) -> Result<()> {
        // Generate 100 one-time PreKeys
        for id in 1..=100u32 {
            let key_pair = KeyPair::generate(&mut rand::rngs::StdRng::from_os_rng());
            let record = PreKeyRecord::new(PreKeyId::from(id), &key_pair);
            self.store.save_pre_key(PreKeyId::from(id), &record).await
                .map_err(|e| anyhow::anyhow!("Failed to save PreKey: {e}"))?;
        }

        // Generate SignedPreKey
        let signed_key_pair = KeyPair::generate(&mut rand::rngs::StdRng::from_os_rng());
        let identity_key_pair = self.store.get_identity_key_pair().await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;

        let signature = identity_key_pair
            .private_key()
            .calculate_signature(signed_key_pair.public_key.serialize().as_ref(), &mut rand::rngs::StdRng::from_os_rng())
            .map_err(|e| anyhow::anyhow!("Failed to sign PreKey: {e}"))?;

        let signed_record = SignedPreKeyRecord::new(
            SignedPreKeyId::from(1),
            Timestamp::from_epoch_millis(
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("time after epoch")
                    .as_millis() as u64
            ),
            &signed_key_pair,
            &signature,
        );
        self.store.save_signed_pre_key(SignedPreKeyId::from(1), &signed_record).await
            .map_err(|e| anyhow::anyhow!("Failed to save SignedPreKey: {e}"))?;

        // Generate KyberPreKey (post-quantum)
        let kyber_key_pair = libsignal_protocol::kem::KeyPair::generate(
            libsignal_protocol::kem::KeyType::Kyber1024,
            &mut rand::rngs::StdRng::from_os_rng(),
        );
        let kyber_signature = identity_key_pair
            .private_key()
            .calculate_signature(kyber_key_pair.public_key.serialize().as_ref(), &mut rand::rngs::StdRng::from_os_rng())
            .map_err(|e| anyhow::anyhow!("Failed to sign KyberPreKey: {e}"))?;

        let kyber_record = KyberPreKeyRecord::new(
            KyberPreKeyId::from(1),
            Timestamp::from_epoch_millis(
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("time after epoch")
                    .as_millis() as u64
            ),
            &kyber_key_pair,
            &kyber_signature,
        );
        self.store.save_kyber_pre_key(KyberPreKeyId::from(1), &kyber_record).await
            .map_err(|e| anyhow::anyhow!("Failed to save KyberPreKey: {e}"))?;

        log::debug!("Generated PreKeys: 100 one-time, 1 signed, 1 Kyber");
        Ok(())
    }

    /// Build a PreKeyBundle for QR code display.
    ///
    /// The bundle contains all public keys needed for a browser to
    /// establish a session with the CLI.
    ///
    /// Automatically selects an available PreKey. If `preferred_prekey_id` is
    /// provided and available, uses that; otherwise finds any available PreKey.
    pub async fn build_prekey_bundle_data(&self, preferred_prekey_id: u32) -> Result<PreKeyBundleData> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let identity_key_pair = self.store.get_identity_key_pair().await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self.store.get_local_registration_id().await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))?;

        // Try preferred ID first, then find any available PreKey
        let prekey_id = if self.store.get_pre_key(PreKeyId::from(preferred_prekey_id)).await.is_ok() {
            preferred_prekey_id
        } else {
            self.store.get_available_prekey_id().await
                .ok_or_else(|| anyhow::anyhow!("No PreKeys available - need to regenerate keys"))?
        };

        log::debug!("Using PreKey {} for bundle ({} remaining)", prekey_id, self.store.prekey_count().await);

        let prekey = self.store.get_pre_key(PreKeyId::from(prekey_id)).await
            .map_err(|e| anyhow::anyhow!("Failed to get PreKey: {e}"))?;
        let signed_prekey = self.store.get_signed_pre_key(SignedPreKeyId::from(1)).await
            .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey: {e}"))?;
        let kyber_prekey = self.store.get_kyber_pre_key(KyberPreKeyId::from(1)).await
            .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey: {e}"))?;

        Ok(PreKeyBundleData {
            version: SIGNAL_PROTOCOL_VERSION,
            hub_id: self.hub_id.clone(),
            registration_id,
            device_id: CLI_DEVICE_ID,
            identity_key: BASE64.encode(identity_key_pair.public_key().serialize()),
            signed_prekey_id: 1,
            signed_prekey: BASE64.encode(signed_prekey.public_key()
                .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey public key: {e}"))?
                .serialize()),
            signed_prekey_signature: BASE64.encode(signed_prekey.signature()
                .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey signature: {e}"))?),
            prekey_id: Some(prekey_id),
            prekey: Some(BASE64.encode(prekey.public_key()
                .map_err(|e| anyhow::anyhow!("Failed to get PreKey public key: {e}"))?
                .serialize())),
            kyber_prekey_id: 1,
            kyber_prekey: BASE64.encode(kyber_prekey.public_key()
                .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey public key: {e}"))?
                .serialize()),
            kyber_prekey_signature: BASE64.encode(kyber_prekey.signature()
                .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey signature: {e}"))?),
        })
    }

    /// Get our identity public key (base64).
    pub async fn identity_key(&self) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let identity = self.store.get_identity_key_pair().await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        Ok(BASE64.encode(identity.public_key().serialize()))
    }

    /// Get our registration ID.
    pub async fn registration_id(&self) -> Result<u32> {
        self.store.get_local_registration_id().await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))
    }

    /// Check if we have a session with a peer.
    pub async fn has_session(&self, peer_identity: &str) -> Result<bool> {
        let address = self.peer_address(peer_identity);
        let session = self.store.load_session(&address).await
            .map_err(|e| anyhow::anyhow!("Failed to load session: {e}"))?;
        Ok(session.is_some())
    }

    /// Encrypt a message for a peer.
    ///
    /// Returns a SignalEnvelope ready for transmission.
    pub async fn encrypt(&mut self, plaintext: &[u8], peer_identity: &str) -> Result<SignalEnvelope> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let address = self.peer_address(peer_identity);

        // Clone store to satisfy borrow checker - clones share Arc data
        let mut session_store = self.store.clone();
        let mut identity_store = self.store.clone();

        let ciphertext = message_encrypt(
            plaintext,
            &address,
            &mut session_store,
            &mut identity_store,
            SystemTime::now(),
            &mut rand::rngs::StdRng::from_os_rng(),
        ).await.map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

        let message_type = match ciphertext.message_type() {
            CiphertextMessageType::PreKey => SignalEnvelope::MSG_TYPE_PREKEY,
            CiphertextMessageType::Whisper => SignalEnvelope::MSG_TYPE_SIGNAL,
            CiphertextMessageType::SenderKey => SignalEnvelope::MSG_TYPE_SENDER_KEY,
            _ => SignalEnvelope::MSG_TYPE_SIGNAL,
        };

        let identity = self.store.get_identity_key_pair().await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self.store.get_local_registration_id().await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))?;

        // Persist session after encryption (ratchet advanced)
        self.store.persist(&self.hub_id).await?;

        Ok(SignalEnvelope {
            version: SIGNAL_PROTOCOL_VERSION,
            message_type,
            ciphertext: BASE64.encode(ciphertext.serialize()),
            sender_identity: BASE64.encode(identity.public_key().serialize()),
            registration_id,
            device_id: CLI_DEVICE_ID,
        })
    }

    /// Decrypt a message from a peer.
    ///
    /// Handles both PreKeySignalMessage (initial) and SignalMessage (ongoing).
    pub async fn decrypt(&mut self, envelope: &SignalEnvelope) -> Result<Vec<u8>> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let ciphertext = BASE64.decode(&envelope.ciphertext)
            .context("Invalid base64 ciphertext")?;

        let peer_address = ProtocolAddress::new(
            envelope.sender_identity.clone(),
            DeviceId::new(envelope.device_id as u8).expect("valid device ID"),
        );

        let plaintext = match envelope.message_type {
            SignalEnvelope::MSG_TYPE_PREKEY => {
                let prekey_message = PreKeySignalMessage::try_from(ciphertext.as_slice())
                    .map_err(|e| anyhow::anyhow!("Invalid PreKeySignalMessage: {e}"))?;

                // Clone store to satisfy borrow checker - clones share Arc data
                let mut session_store = self.store.clone();
                let mut identity_store = self.store.clone();
                let mut prekey_store = self.store.clone();
                let signed_prekey_store = self.store.clone();
                let mut kyber_store = self.store.clone();

                message_decrypt_prekey(
                    &prekey_message,
                    &peer_address,
                    &mut session_store,
                    &mut identity_store,
                    &mut prekey_store,
                    &signed_prekey_store,
                    &mut kyber_store,
                    &mut rand::rngs::StdRng::from_os_rng(),
                ).await.map_err(|e| anyhow::anyhow!("PreKey decryption failed: {e}"))?
            }
            SignalEnvelope::MSG_TYPE_SIGNAL => {
                let signal_message = SignalMessage::try_from(ciphertext.as_slice())
                    .map_err(|e| anyhow::anyhow!("Invalid SignalMessage: {e}"))?;

                // Clone store to satisfy borrow checker - clones share Arc data
                let mut session_store = self.store.clone();
                let mut identity_store = self.store.clone();

                message_decrypt_signal(
                    &signal_message,
                    &peer_address,
                    &mut session_store,
                    &mut identity_store,
                    &mut rand::rngs::StdRng::from_os_rng(),
                ).await.map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))?
            }
            SignalEnvelope::MSG_TYPE_SENDER_KEY => {
                // SenderKey messages are for group - browser shouldn't send these
                anyhow::bail!("Unexpected SenderKeyMessage from browser");
            }
            other => anyhow::bail!("Unknown message type: {other}"),
        };

        // Persist session after decryption (ratchet advanced)
        self.store.persist(&self.hub_id).await?;

        Ok(plaintext)
    }

    /// Create a SenderKey distribution message for group broadcasts.
    ///
    /// This should be sent to each browser via their individual session
    /// before using group_encrypt for broadcasts.
    pub async fn create_sender_key_distribution(&mut self) -> Result<Vec<u8>> {
        let distribution = create_sender_key_distribution_message(
            &self.our_address,
            self.group_id,
            &mut self.store,
            &mut rand::rngs::StdRng::from_os_rng(),
        ).await.map_err(|e| anyhow::anyhow!("Failed to create SenderKey distribution: {e}"))?;

        Ok(distribution.serialized().to_vec())
    }

    /// Encrypt a message for all group members using SenderKey.
    ///
    /// This is more efficient than encrypting individually when broadcasting
    /// to multiple browsers (e.g., terminal output).
    pub async fn group_encrypt(&mut self, plaintext: &[u8]) -> Result<SignalEnvelope> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let ciphertext = group_encrypt(
            &mut self.store,
            &self.our_address,
            self.group_id,
            plaintext,
            &mut rand::rngs::StdRng::from_os_rng(),
        ).await.map_err(|e| anyhow::anyhow!("Group encryption failed: {e}"))?;

        let identity = self.store.get_identity_key_pair().await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self.store.get_local_registration_id().await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))?;

        Ok(SignalEnvelope {
            version: SIGNAL_PROTOCOL_VERSION,
            message_type: SignalEnvelope::MSG_TYPE_SENDER_KEY,
            ciphertext: BASE64.encode(ciphertext.serialized()),
            sender_identity: BASE64.encode(identity.public_key().serialize()),
            registration_id,
            device_id: CLI_DEVICE_ID,
        })
    }

    /// Derive a stable address name from hub_id.
    fn derive_address_name(hub_id: &str) -> String {
        let hash = Sha256::digest(format!("signal-address-{hub_id}").as_bytes());
        // Format first 16 bytes as hex
        hash[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Derive a stable group ID from hub_id.
    fn derive_group_id(hub_id: &str) -> uuid::Uuid {
        let hash = Sha256::digest(format!("signal-group-{hub_id}").as_bytes());
        // Use first 16 bytes as UUID (UUID is 128 bits = 16 bytes)
        uuid::Uuid::from_slice(&hash[..16]).expect("16 bytes make a valid UUID")
    }

    /// Create a ProtocolAddress for a peer from their identity key.
    fn peer_address(&self, peer_identity: &str) -> ProtocolAddress {
        // Use the peer's identity key as their address name
        // Device ID 2+ for browsers (CLI is 1)
        ProtocolAddress::new(
            peer_identity.to_string(),
            DeviceId::new(2u8).expect("valid device ID"),
        )
    }

    /// Save the store to disk.
    pub async fn persist(&self) -> Result<()> {
        self.store.persist(&self.hub_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation() {
        let manager = SignalProtocolManager::new("test-hub").await.unwrap();
        assert!(!manager.hub_id.is_empty());
    }

    #[tokio::test]
    async fn test_prekey_bundle_generation() {
        let manager = SignalProtocolManager::new("test-hub-bundle").await.unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        assert_eq!(bundle.version, SIGNAL_PROTOCOL_VERSION);
        assert_eq!(bundle.device_id, CLI_DEVICE_ID);
        assert!(!bundle.identity_key.is_empty());
        assert!(!bundle.signed_prekey.is_empty());
        assert!(!bundle.kyber_prekey.is_empty());
    }
}
