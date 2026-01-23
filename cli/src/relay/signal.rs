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
    create_sender_key_distribution_message,
    group_encrypt,
    message_decrypt_prekey,
    message_decrypt_signal,
    // Operations
    message_encrypt,
    // Messages
    CiphertextMessageType,
    DeviceId,
    // Session and keys
    GenericSignedPreKey,
    // Core types
    KeyPair,
    KyberPreKeyId,
    KyberPreKeyRecord,
    KyberPreKeyStore,
    PreKeyId,
    PreKeyRecord,
    PreKeySignalMessage,
    PreKeyStore,
    ProtocolAddress,
    // Stores
    SessionStore,
    SignalMessage,
    SignedPreKeyId,
    SignedPreKeyRecord,
    SignedPreKeyStore,
    Timestamp,
};
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

/// Binary format constants for PreKeyBundleData.
///
/// Total size: 1813 bytes (fits in QR alphanumeric mode with Base32).
///
/// # Browser-side Decoding
///
/// The browser receives an uppercase URL like:
/// ```text
/// HTTPS://BOTSTER.DEV/H/123#GEZDGNBVGY3TQOJQ...
/// ```
///
/// To decode in JavaScript:
/// ```javascript
/// // 1. Get fragment (without #)
/// const fragment = window.location.hash.slice(1);
///
/// // 2. Decode Base32 to bytes
/// const bytes = base32Decode(fragment); // Use a Base32 library (no padding)
///
/// // 3. Parse binary format using these offsets:
/// const bundle = {
///     version: bytes[0],
///     registration_id: new DataView(bytes.buffer).getUint32(1, true), // little-endian
///     identity_key: bytes.slice(5, 38),
///     signed_prekey_id: new DataView(bytes.buffer).getUint32(38, true),
///     signed_prekey: bytes.slice(42, 75),
///     signed_prekey_signature: bytes.slice(75, 139),
///     prekey_id: new DataView(bytes.buffer).getUint32(139, true),
///     prekey: bytes.slice(143, 176),
///     kyber_prekey_id: new DataView(bytes.buffer).getUint32(176, true),
///     kyber_prekey: bytes.slice(180, 1749),
///     kyber_prekey_signature: bytes.slice(1749, 1813),
/// };
/// ```
///
/// Recommended Base32 library: `hi-base32` (npm) or manual decode.
pub mod binary_format {
    //! Binary format constants for PreKeyBundle serialization.
    //!
    //! These define byte offsets and sizes for the compact binary format
    //! used in QR codes. Total size: 1813 bytes.

    /// Byte offset: format version (1 byte).
    pub const VERSION_OFFSET: usize = 0;
    /// Byte offset: registration ID (4 bytes LE).
    pub const REGISTRATION_ID_OFFSET: usize = 1;
    /// Byte offset: identity key (33 bytes).
    pub const IDENTITY_KEY_OFFSET: usize = 5;
    /// Byte offset: signed prekey ID (4 bytes LE).
    pub const SIGNED_PREKEY_ID_OFFSET: usize = 38;
    /// Byte offset: signed prekey (33 bytes).
    pub const SIGNED_PREKEY_OFFSET: usize = 42;
    /// Byte offset: signed prekey signature (64 bytes).
    pub const SIGNED_PREKEY_SIG_OFFSET: usize = 75;
    /// Byte offset: one-time prekey ID (4 bytes LE).
    pub const PREKEY_ID_OFFSET: usize = 139;
    /// Byte offset: one-time prekey (33 bytes).
    pub const PREKEY_OFFSET: usize = 143;
    /// Byte offset: Kyber prekey ID (4 bytes LE).
    pub const KYBER_PREKEY_ID_OFFSET: usize = 176;
    /// Byte offset: Kyber prekey (1569 bytes).
    pub const KYBER_PREKEY_OFFSET: usize = 180;
    /// Byte offset: Kyber prekey signature (64 bytes). Equals 180 + 1569.
    pub const KYBER_PREKEY_SIG_OFFSET: usize = 1749;
    /// Total binary bundle size in bytes. Equals 1749 + 64.
    pub const TOTAL_SIZE: usize = 1813;

    /// Size of identity key in bytes.
    pub const IDENTITY_KEY_SIZE: usize = 33;
    /// Size of signed prekey in bytes.
    pub const SIGNED_PREKEY_SIZE: usize = 33;
    /// Size of signed prekey signature in bytes.
    pub const SIGNED_PREKEY_SIG_SIZE: usize = 64;
    /// Size of one-time prekey in bytes.
    pub const PREKEY_SIZE: usize = 33;
    /// Size of Kyber1024 public key in bytes (includes type byte).
    pub const KYBER_PREKEY_SIZE: usize = 1569;
    /// Size of Kyber prekey signature in bytes.
    pub const KYBER_PREKEY_SIG_SIZE: usize = 64;
}

impl PreKeyBundleData {
    /// Serialize to compact binary format for QR codes.
    ///
    /// Binary format (1812 bytes total):
    /// - version: 1 byte
    /// - registration_id: 4 bytes (LE)
    /// - identity_key: 33 bytes
    /// - signed_prekey_id: 4 bytes (LE)
    /// - signed_prekey: 33 bytes
    /// - signed_prekey_signature: 64 bytes
    /// - prekey_id: 4 bytes (LE, 0 if none)
    /// - prekey: 33 bytes (zeros if none)
    /// - kyber_prekey_id: 4 bytes (LE)
    /// - kyber_prekey: 1568 bytes
    /// - kyber_prekey_signature: 64 bytes
    ///
    /// Note: hub_id and device_id are NOT included - they come from URL path.
    pub fn to_binary(&self) -> Result<Vec<u8>> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use binary_format::*;

        let mut buf = vec![0u8; TOTAL_SIZE];

        // Version
        buf[VERSION_OFFSET] = self.version;

        // Registration ID (little-endian)
        buf[REGISTRATION_ID_OFFSET..REGISTRATION_ID_OFFSET + 4]
            .copy_from_slice(&self.registration_id.to_le_bytes());

        // Identity key (decode from base64)
        let identity_key = STANDARD
            .decode(&self.identity_key)
            .map_err(|e| anyhow::anyhow!("Invalid identity_key base64: {e}"))?;
        if identity_key.len() != IDENTITY_KEY_SIZE {
            return Err(anyhow::anyhow!(
                "identity_key wrong size: {} != {}",
                identity_key.len(),
                IDENTITY_KEY_SIZE
            ));
        }
        buf[IDENTITY_KEY_OFFSET..IDENTITY_KEY_OFFSET + IDENTITY_KEY_SIZE]
            .copy_from_slice(&identity_key);

        // Signed PreKey ID
        buf[SIGNED_PREKEY_ID_OFFSET..SIGNED_PREKEY_ID_OFFSET + 4]
            .copy_from_slice(&self.signed_prekey_id.to_le_bytes());

        // Signed PreKey
        let signed_prekey = STANDARD
            .decode(&self.signed_prekey)
            .map_err(|e| anyhow::anyhow!("Invalid signed_prekey base64: {e}"))?;
        if signed_prekey.len() != SIGNED_PREKEY_SIZE {
            return Err(anyhow::anyhow!(
                "signed_prekey wrong size: {} != {}",
                signed_prekey.len(),
                SIGNED_PREKEY_SIZE
            ));
        }
        buf[SIGNED_PREKEY_OFFSET..SIGNED_PREKEY_OFFSET + SIGNED_PREKEY_SIZE]
            .copy_from_slice(&signed_prekey);

        // Signed PreKey signature
        let signed_prekey_sig = STANDARD
            .decode(&self.signed_prekey_signature)
            .map_err(|e| anyhow::anyhow!("Invalid signed_prekey_signature base64: {e}"))?;
        if signed_prekey_sig.len() != SIGNED_PREKEY_SIG_SIZE {
            return Err(anyhow::anyhow!(
                "signed_prekey_signature wrong size: {} != {}",
                signed_prekey_sig.len(),
                SIGNED_PREKEY_SIG_SIZE
            ));
        }
        buf[SIGNED_PREKEY_SIG_OFFSET..SIGNED_PREKEY_SIG_OFFSET + SIGNED_PREKEY_SIG_SIZE]
            .copy_from_slice(&signed_prekey_sig);

        // PreKey ID and PreKey (optional)
        if let (Some(id), Some(ref pk)) = (self.prekey_id, &self.prekey) {
            buf[PREKEY_ID_OFFSET..PREKEY_ID_OFFSET + 4].copy_from_slice(&id.to_le_bytes());
            let prekey = STANDARD
                .decode(pk)
                .map_err(|e| anyhow::anyhow!("Invalid prekey base64: {e}"))?;
            if prekey.len() != PREKEY_SIZE {
                return Err(anyhow::anyhow!(
                    "prekey wrong size: {} != {}",
                    prekey.len(),
                    PREKEY_SIZE
                ));
            }
            buf[PREKEY_OFFSET..PREKEY_OFFSET + PREKEY_SIZE].copy_from_slice(&prekey);
        }
        // else: already zeros

        // Kyber PreKey ID
        buf[KYBER_PREKEY_ID_OFFSET..KYBER_PREKEY_ID_OFFSET + 4]
            .copy_from_slice(&self.kyber_prekey_id.to_le_bytes());

        // Kyber PreKey
        let kyber_prekey = STANDARD
            .decode(&self.kyber_prekey)
            .map_err(|e| anyhow::anyhow!("Invalid kyber_prekey base64: {e}"))?;
        if kyber_prekey.len() != KYBER_PREKEY_SIZE {
            return Err(anyhow::anyhow!(
                "kyber_prekey wrong size: {} != {}",
                kyber_prekey.len(),
                KYBER_PREKEY_SIZE
            ));
        }
        buf[KYBER_PREKEY_OFFSET..KYBER_PREKEY_OFFSET + KYBER_PREKEY_SIZE]
            .copy_from_slice(&kyber_prekey);

        // Kyber PreKey signature
        let kyber_prekey_sig = STANDARD
            .decode(&self.kyber_prekey_signature)
            .map_err(|e| anyhow::anyhow!("Invalid kyber_prekey_signature base64: {e}"))?;
        if kyber_prekey_sig.len() != KYBER_PREKEY_SIG_SIZE {
            return Err(anyhow::anyhow!(
                "kyber_prekey_signature wrong size: {} != {}",
                kyber_prekey_sig.len(),
                KYBER_PREKEY_SIG_SIZE
            ));
        }
        buf[KYBER_PREKEY_SIG_OFFSET..KYBER_PREKEY_SIG_OFFSET + KYBER_PREKEY_SIG_SIZE]
            .copy_from_slice(&kyber_prekey_sig);

        Ok(buf)
    }

    /// Deserialize from compact binary format.
    ///
    /// Note: hub_id is set to empty string (comes from URL path).
    /// device_id is set to CLI_DEVICE_ID (always 1).
    pub fn from_binary(bytes: &[u8]) -> Result<Self> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use binary_format::*;

        if bytes.len() != TOTAL_SIZE {
            return Err(anyhow::anyhow!(
                "Binary bundle wrong size: {} != {}",
                bytes.len(),
                TOTAL_SIZE
            ));
        }

        let version = bytes[VERSION_OFFSET];

        let registration_id = u32::from_le_bytes(
            bytes[REGISTRATION_ID_OFFSET..REGISTRATION_ID_OFFSET + 4]
                .try_into()
                .unwrap(),
        );

        let identity_key =
            STANDARD.encode(&bytes[IDENTITY_KEY_OFFSET..IDENTITY_KEY_OFFSET + IDENTITY_KEY_SIZE]);

        let signed_prekey_id = u32::from_le_bytes(
            bytes[SIGNED_PREKEY_ID_OFFSET..SIGNED_PREKEY_ID_OFFSET + 4]
                .try_into()
                .unwrap(),
        );

        let signed_prekey = STANDARD
            .encode(&bytes[SIGNED_PREKEY_OFFSET..SIGNED_PREKEY_OFFSET + SIGNED_PREKEY_SIZE]);

        let signed_prekey_signature = STANDARD.encode(
            &bytes[SIGNED_PREKEY_SIG_OFFSET..SIGNED_PREKEY_SIG_OFFSET + SIGNED_PREKEY_SIG_SIZE],
        );

        let prekey_id_raw = u32::from_le_bytes(
            bytes[PREKEY_ID_OFFSET..PREKEY_ID_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        let prekey_bytes = &bytes[PREKEY_OFFSET..PREKEY_OFFSET + PREKEY_SIZE];
        let (prekey_id, prekey) = if prekey_id_raw == 0 && prekey_bytes.iter().all(|&b| b == 0) {
            (None, None)
        } else {
            (Some(prekey_id_raw), Some(STANDARD.encode(prekey_bytes)))
        };

        let kyber_prekey_id = u32::from_le_bytes(
            bytes[KYBER_PREKEY_ID_OFFSET..KYBER_PREKEY_ID_OFFSET + 4]
                .try_into()
                .unwrap(),
        );

        let kyber_prekey =
            STANDARD.encode(&bytes[KYBER_PREKEY_OFFSET..KYBER_PREKEY_OFFSET + KYBER_PREKEY_SIZE]);

        let kyber_prekey_signature = STANDARD.encode(
            &bytes[KYBER_PREKEY_SIG_OFFSET..KYBER_PREKEY_SIG_OFFSET + KYBER_PREKEY_SIG_SIZE],
        );

        Ok(Self {
            version,
            hub_id: String::new(), // Comes from URL path
            registration_id,
            device_id: CLI_DEVICE_ID,
            identity_key,
            signed_prekey_id,
            signed_prekey,
            signed_prekey_signature,
            prekey_id,
            prekey,
            kyber_prekey_id,
            kyber_prekey,
            kyber_prekey_signature,
        })
    }
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
    /// Creates identity keypair immediately but defers PreKey generation
    /// until first bundle request (lazy initialization for faster startup).
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

        log::info!(
            "Created new SignalProtocolManager for hub {} (keys deferred)",
            &hub_id[..hub_id.len().min(8)]
        );

        Ok(Self {
            store,
            hub_id: hub_id.to_string(),
            our_address,
            group_id,
        })
    }

    /// Load existing manager or create new one.
    ///
    /// Key generation is deferred until first bundle request for fast startup.
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

    /// Ensure keys are ready for bundle generation.
    ///
    /// Generates PreKeys, SignedPreKey, and KyberPreKey if not already present.
    /// This is called lazily when the first connection code is requested.
    pub async fn ensure_keys_ready(&mut self) -> Result<()> {
        // Check if we already have keys
        if self
            .store
            .get_signed_pre_key(SignedPreKeyId::from(1))
            .await
            .is_ok()
        {
            log::debug!("Signal keys already present");
            return Ok(());
        }

        log::info!("Generating Signal Protocol keys (first connection code request)...");
        self.generate_prekeys().await?;

        // Persist the new keys so they survive CLI restarts
        self.store.persist(&self.hub_id).await?;

        Ok(())
    }

    /// Number of one-time PreKeys to generate.
    ///
    /// Signal servers generate 100 for distribution to many clients.
    /// For a local CLI connecting 1-2 browsers, 10 is plenty.
    const PREKEY_COUNT: u32 = 10;

    /// Generate PreKeys for session establishment.
    async fn generate_prekeys(&mut self) -> Result<()> {
        let start = std::time::Instant::now();

        // Generate one-time PreKeys (reduced count for local CLI use case)
        for id in 1..=Self::PREKEY_COUNT {
            let key_pair = KeyPair::generate(&mut rand::rngs::StdRng::from_os_rng());
            let record = PreKeyRecord::new(PreKeyId::from(id), &key_pair);
            self.store
                .save_pre_key(PreKeyId::from(id), &record)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to save PreKey: {e}"))?;
        }
        log::debug!(
            "Generated {} PreKeys in {:?}",
            Self::PREKEY_COUNT,
            start.elapsed()
        );

        // Generate SignedPreKey
        let signed_start = std::time::Instant::now();
        let signed_key_pair = KeyPair::generate(&mut rand::rngs::StdRng::from_os_rng());
        let identity_key_pair = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;

        let signature = identity_key_pair
            .private_key()
            .calculate_signature(
                signed_key_pair.public_key.serialize().as_ref(),
                &mut rand::rngs::StdRng::from_os_rng(),
            )
            .map_err(|e| anyhow::anyhow!("Failed to sign PreKey: {e}"))?;

        let signed_record = SignedPreKeyRecord::new(
            SignedPreKeyId::from(1),
            Timestamp::from_epoch_millis(
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("time after epoch")
                    .as_millis() as u64,
            ),
            &signed_key_pair,
            &signature,
        );
        self.store
            .save_signed_pre_key(SignedPreKeyId::from(1), &signed_record)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to save SignedPreKey: {e}"))?;
        log::debug!("Generated SignedPreKey in {:?}", signed_start.elapsed());

        // Generate KyberPreKey (post-quantum) - this is CPU-intensive
        let kyber_start = std::time::Instant::now();
        let kyber_key_pair = libsignal_protocol::kem::KeyPair::generate(
            libsignal_protocol::kem::KeyType::Kyber1024,
            &mut rand::rngs::StdRng::from_os_rng(),
        );
        let kyber_signature = identity_key_pair
            .private_key()
            .calculate_signature(
                kyber_key_pair.public_key.serialize().as_ref(),
                &mut rand::rngs::StdRng::from_os_rng(),
            )
            .map_err(|e| anyhow::anyhow!("Failed to sign KyberPreKey: {e}"))?;

        let kyber_record = KyberPreKeyRecord::new(
            KyberPreKeyId::from(1),
            Timestamp::from_epoch_millis(
                SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("time after epoch")
                    .as_millis() as u64,
            ),
            &kyber_key_pair,
            &kyber_signature,
        );
        self.store
            .save_kyber_pre_key(KyberPreKeyId::from(1), &kyber_record)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to save KyberPreKey: {e}"))?;
        log::debug!("Generated KyberPreKey in {:?}", kyber_start.elapsed());

        log::info!(
            "Signal key generation complete: {} PreKeys, 1 SignedPreKey, 1 KyberPreKey in {:?}",
            Self::PREKEY_COUNT,
            start.elapsed()
        );
        Ok(())
    }

    /// Build a PreKeyBundle for QR code display.
    ///
    /// The bundle contains all public keys needed for a browser to
    /// establish a session with the CLI.
    ///
    /// Automatically selects an available PreKey. If `preferred_prekey_id` is
    /// provided and available, uses that; otherwise finds any available PreKey.
    ///
    /// Keys are generated lazily on first call for faster startup.
    pub async fn build_prekey_bundle_data(
        &mut self,
        preferred_prekey_id: u32,
    ) -> Result<PreKeyBundleData> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        // Ensure keys are generated (lazy initialization)
        self.ensure_keys_ready().await?;

        let identity_key_pair = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self
            .store
            .get_local_registration_id()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))?;

        // Try preferred ID first, then find any available PreKey
        let prekey_id = if self
            .store
            .get_pre_key(PreKeyId::from(preferred_prekey_id))
            .await
            .is_ok()
        {
            preferred_prekey_id
        } else {
            self.store
                .get_available_prekey_id()
                .await
                .ok_or_else(|| anyhow::anyhow!("No PreKeys available - need to regenerate keys"))?
        };

        log::debug!(
            "Using PreKey {} for bundle ({} remaining)",
            prekey_id,
            self.store.prekey_count().await
        );

        let prekey = self
            .store
            .get_pre_key(PreKeyId::from(prekey_id))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get PreKey: {e}"))?;
        let signed_prekey = self
            .store
            .get_signed_pre_key(SignedPreKeyId::from(1))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey: {e}"))?;
        let kyber_prekey = self
            .store
            .get_kyber_pre_key(KyberPreKeyId::from(1))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey: {e}"))?;

        Ok(PreKeyBundleData {
            version: SIGNAL_PROTOCOL_VERSION,
            hub_id: self.hub_id.clone(),
            registration_id,
            device_id: CLI_DEVICE_ID,
            identity_key: BASE64.encode(identity_key_pair.public_key().serialize()),
            signed_prekey_id: 1,
            signed_prekey: BASE64.encode(
                signed_prekey
                    .public_key()
                    .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey public key: {e}"))?
                    .serialize(),
            ),
            signed_prekey_signature: BASE64.encode(
                signed_prekey
                    .signature()
                    .map_err(|e| anyhow::anyhow!("Failed to get SignedPreKey signature: {e}"))?,
            ),
            prekey_id: Some(prekey_id),
            prekey: Some(
                BASE64.encode(
                    prekey
                        .public_key()
                        .map_err(|e| anyhow::anyhow!("Failed to get PreKey public key: {e}"))?
                        .serialize(),
                ),
            ),
            kyber_prekey_id: 1,
            kyber_prekey: BASE64.encode(
                kyber_prekey
                    .public_key()
                    .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey public key: {e}"))?
                    .serialize(),
            ),
            kyber_prekey_signature: BASE64.encode(
                kyber_prekey
                    .signature()
                    .map_err(|e| anyhow::anyhow!("Failed to get KyberPreKey signature: {e}"))?,
            ),
        })
    }

    /// Get our identity public key (base64).
    pub async fn identity_key(&self) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

        let identity = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        Ok(BASE64.encode(identity.public_key().serialize()))
    }

    /// Get our registration ID.
    pub async fn registration_id(&self) -> Result<u32> {
        self.store
            .get_local_registration_id()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get registration ID: {e}"))
    }

    /// Get the next available PreKey ID for bundle generation.
    /// Returns None if all PreKeys have been consumed.
    pub async fn next_prekey_id(&self) -> Option<u32> {
        self.store.get_available_prekey_id().await
    }

    /// Check if we have a session with a peer.
    pub async fn has_session(&self, peer_identity: &str) -> Result<bool> {
        let address = self.peer_address(peer_identity);
        let session = self
            .store
            .load_session(&address)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load session: {e}"))?;
        Ok(session.is_some())
    }

    /// Encrypt a message for a peer.
    ///
    /// Returns a SignalEnvelope ready for transmission.
    pub async fn encrypt(
        &mut self,
        plaintext: &[u8],
        peer_identity: &str,
    ) -> Result<SignalEnvelope> {
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
        )
        .await
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

        let message_type = match ciphertext.message_type() {
            CiphertextMessageType::PreKey => SignalEnvelope::MSG_TYPE_PREKEY,
            CiphertextMessageType::Whisper => SignalEnvelope::MSG_TYPE_SIGNAL,
            CiphertextMessageType::SenderKey => SignalEnvelope::MSG_TYPE_SENDER_KEY,
            _ => SignalEnvelope::MSG_TYPE_SIGNAL,
        };

        let identity = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self
            .store
            .get_local_registration_id()
            .await
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

        log::debug!(
            "Decrypting message: type={}, sender={}, device={}",
            envelope.message_type,
            &envelope.sender_identity[..envelope.sender_identity.len().min(16)],
            envelope.device_id
        );

        let ciphertext = BASE64
            .decode(&envelope.ciphertext)
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
                )
                .await
                .map_err(|e| anyhow::anyhow!("PreKey decryption failed: {e}"))?
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
                )
                .await
                .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))?
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
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create SenderKey distribution: {e}"))?;

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
        )
        .await
        .map_err(|e| anyhow::anyhow!("Group encryption failed: {e}"))?;

        let identity = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get identity: {e}"))?;
        let registration_id = self
            .store
            .get_local_registration_id()
            .await
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
        let mut manager = SignalProtocolManager::new("test-hub-bundle").await.unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        assert_eq!(bundle.version, SIGNAL_PROTOCOL_VERSION);
        assert_eq!(bundle.device_id, CLI_DEVICE_ID);
        assert!(!bundle.identity_key.is_empty());
        assert!(!bundle.signed_prekey.is_empty());
        assert!(!bundle.kyber_prekey.is_empty());
    }

    #[tokio::test]
    async fn test_prekey_bundle_url_size() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

        let mut manager = SignalProtocolManager::new("test-hub-size").await.unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        let json = serde_json::to_string(&bundle).unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
        let url = format!("https://example.com/hubs/abc123#{}", encoded);

        println!("Bundle JSON size: {} chars", json.len());
        println!("Bundle base64 size: {} chars", encoded.len());
        println!("Full URL size: {} chars", url.len());
        println!("Kyber key size: {} chars", bundle.kyber_prekey.len());

        // DOCUMENTED LIMITATION: Post-quantum Kyber keys (~2092 chars) make the URL
        // too long for QR codes (max ~2900 chars). Full URL is ~3500+ chars.
        // The UI handles this by showing "URL too long for QR code" and offering
        // the copy URL option instead.
        assert!(
            url.len() > 2900,
            "With Kyber keys, URL should exceed QR capacity - if this fails, QR codes might work now!"
        );

        // Verify Kyber is the main contributor
        assert!(
            bundle.kyber_prekey.len() > 2000,
            "Kyber key should be the largest component"
        );
    }

    #[tokio::test]
    async fn test_qr_size_options() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

        let mut manager = SignalProtocolManager::new("test-hub-qr-options")
            .await
            .unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();
        let json = serde_json::to_string(&bundle).unwrap();

        println!("\n=== QR Code Size Analysis ===\n");

        // Current approach (JSON + base64)
        let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
        println!("Current (JSON + base64): {} chars", encoded.len());
        println!("QR max capacity:         ~2900 chars");
        println!(
            "Overflow:                {} chars\n",
            encoded.len() as i32 - 2900
        );

        // Size breakdown
        println!("=== Component Sizes (base64) ===");
        println!(
            "Kyber prekey:       {} chars (the problem)",
            bundle.kyber_prekey.len()
        );
        println!(
            "Kyber signature:    {} chars",
            bundle.kyber_prekey_signature.len()
        );
        println!("Identity key:       {} chars", bundle.identity_key.len());
        println!("Signed prekey:      {} chars", bundle.signed_prekey.len());
        println!(
            "Signed prekey sig:  {} chars",
            bundle.signed_prekey_signature.len()
        );
        if let Some(ref pk) = bundle.prekey {
            println!("One-time prekey:    {} chars", pk.len());
        }

        // Raw byte sizes (pre-base64)
        println!("\n=== Raw Byte Sizes ===");
        println!(
            "Kyber1024 public key:  1568 bytes → {} base64 chars",
            (1568 * 4 + 2) / 3
        );
        println!("X25519 public key:     32 bytes → 43 base64 chars");
        println!("Ed25519 signature:     64 bytes → 86 base64 chars");

        println!("\n=== Viable Options (keeping Kyber) ===");
        println!("1. Server relay: QR has short URL, browser fetches bundle from server");
        println!("   - QR: https://botster.dev/c/abc123 (~35 chars)");
        println!("   - Server stores bundle ephemerally (5 min TTL)");
        println!("   - Tradeoff: requires server, but bundle stays E2E encrypted");
        println!("");
        println!("2. Animated QR sequence: Display 2 QR codes alternating");
        println!("   - Split bundle across 2 QRs (~1770 chars each)");
        println!("   - App scans both and reassembles");
        println!("   - Tradeoff: worse UX, needs custom scanner logic");

        // Note: compression doesn't help - crypto keys are high-entropy random data
        // that actually gets LARGER when you try to compress it

        assert!(
            encoded.len() > 2900,
            "Confirming Kyber bundle exceeds QR capacity"
        );
    }

    // ============ TDD: Binary Bundle Format ============
    // These tests define the expected behavior before implementation

    #[tokio::test]
    async fn test_binary_bundle_round_trip() {
        let mut manager = SignalProtocolManager::new("test-hub-binary").await.unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        // Serialize to binary
        let bytes = bundle.to_binary().expect("serialization should succeed");

        // Deserialize back
        let restored =
            PreKeyBundleData::from_binary(&bytes).expect("deserialization should succeed");

        // All fields should match (except hub_id which is not in binary format)
        assert_eq!(bundle.version, restored.version);
        assert_eq!(bundle.registration_id, restored.registration_id);
        assert_eq!(bundle.device_id, restored.device_id);
        assert_eq!(bundle.identity_key, restored.identity_key);
        assert_eq!(bundle.signed_prekey_id, restored.signed_prekey_id);
        assert_eq!(bundle.signed_prekey, restored.signed_prekey);
        assert_eq!(
            bundle.signed_prekey_signature,
            restored.signed_prekey_signature
        );
        assert_eq!(bundle.prekey_id, restored.prekey_id);
        assert_eq!(bundle.prekey, restored.prekey);
        assert_eq!(bundle.kyber_prekey_id, restored.kyber_prekey_id);
        assert_eq!(bundle.kyber_prekey, restored.kyber_prekey);
        assert_eq!(
            bundle.kyber_prekey_signature,
            restored.kyber_prekey_signature
        );
    }

    #[tokio::test]
    async fn test_binary_bundle_size() {
        let mut manager = SignalProtocolManager::new("test-hub-binary-size")
            .await
            .unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        let bytes = bundle.to_binary().expect("serialization should succeed");

        // Expected size: 1813 bytes
        // version(1) + reg_id(4) + identity(33) + spk_id(4) + spk(33) + spk_sig(64)
        // + pk_id(4) + pk(33) + kpk_id(4) + kpk(1569) + kpk_sig(64) = 1813
        println!("Binary bundle size: {} bytes", bytes.len());
        assert_eq!(
            bytes.len(),
            1813,
            "Binary bundle should be exactly 1813 bytes"
        );
    }

    #[tokio::test]
    async fn test_binary_bundle_fits_in_qr_with_base32() {
        use data_encoding::BASE32_NOPAD;

        let mut manager = SignalProtocolManager::new("test-hub-qr-fit").await.unwrap();
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        let bytes = bundle.to_binary().expect("serialization should succeed");
        let base32 = BASE32_NOPAD.encode(&bytes);

        // Base32 should be all uppercase (for QR alphanumeric mode)
        assert!(
            base32
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "Base32 should be uppercase alphanumeric"
        );

        // Full URL with hub ID
        let url = format!("HTTPS://BOTSTER.DEV/H/123#{}", base32);

        println!("Binary size:  {} bytes", bytes.len());
        println!("Base32 size:  {} chars", base32.len());
        println!("Full URL:     {} chars", url.len());
        println!("QR capacity:  4296 chars (alphanumeric mode)");
        println!("Headroom:     {} chars", 4296 - url.len() as i32);

        // Must fit in QR alphanumeric capacity (4296 chars for version 40-L)
        assert!(url.len() < 4296, "URL should fit in QR alphanumeric mode");

        // Should be well under the limit
        assert!(url.len() < 3000, "URL should be under 3000 chars");
    }

    #[test]
    fn test_binary_format_is_deterministic() {
        // Same input should produce same output
        use base64::{engine::general_purpose::STANDARD, Engine};

        // Create a bundle with known values
        let bundle = PreKeyBundleData {
            version: 4,
            hub_id: "ignored".to_string(), // Not in binary format
            registration_id: 12345,
            device_id: 1,
            identity_key: STANDARD.encode(&[1u8; 33]),
            signed_prekey_id: 1,
            signed_prekey: STANDARD.encode(&[2u8; 33]),
            signed_prekey_signature: STANDARD.encode(&[3u8; 64]),
            prekey_id: Some(1),
            prekey: Some(STANDARD.encode(&[4u8; 33])),
            kyber_prekey_id: 1,
            kyber_prekey: STANDARD.encode(&[5u8; 1569]),
            kyber_prekey_signature: STANDARD.encode(&[6u8; 64]),
        };

        let bytes1 = bundle.to_binary().unwrap();
        let bytes2 = bundle.to_binary().unwrap();

        assert_eq!(
            bytes1, bytes2,
            "Binary serialization should be deterministic"
        );
    }

    #[tokio::test]
    async fn test_lazy_key_generation() {
        // Create manager - should NOT generate keys yet
        let mut manager = SignalProtocolManager::new("test-hub-lazy").await.unwrap();

        // Keys should not exist initially
        let has_signed_key = manager
            .store
            .get_signed_pre_key(SignedPreKeyId::from(1))
            .await
            .is_ok();
        assert!(
            !has_signed_key,
            "SignedPreKey should not exist before bundle request"
        );

        // Request bundle - this should trigger key generation
        let bundle = manager.build_prekey_bundle_data(1).await.unwrap();
        assert!(
            !bundle.identity_key.is_empty(),
            "Bundle should have identity key"
        );
        assert!(
            !bundle.signed_prekey.is_empty(),
            "Bundle should have signed prekey"
        );
        assert!(
            !bundle.kyber_prekey.is_empty(),
            "Bundle should have kyber prekey"
        );

        // Keys should now exist
        let has_signed_key_after = manager
            .store
            .get_signed_pre_key(SignedPreKeyId::from(1))
            .await
            .is_ok();
        assert!(
            has_signed_key_after,
            "SignedPreKey should exist after bundle request"
        );

        // Second bundle request should NOT regenerate keys
        let bundle2 = manager.build_prekey_bundle_data(1).await.unwrap();
        assert_eq!(
            bundle.identity_key, bundle2.identity_key,
            "Identity should be same on second request"
        );
        assert_eq!(
            bundle.signed_prekey, bundle2.signed_prekey,
            "SignedPreKey should be same on second request"
        );
    }

    #[tokio::test]
    async fn test_loaded_store_has_keys_immediately() {
        let hub_id = "test-hub-loaded-keys";

        // Create and populate a manager
        let mut manager = SignalProtocolManager::new(hub_id).await.unwrap();
        let _bundle = manager.build_prekey_bundle_data(1).await.unwrap();

        // Persist and reload
        manager.store.persist(hub_id).await.unwrap();
        let loaded_manager = SignalProtocolManager::load_or_create(hub_id).await.unwrap();

        // Loaded store should already have keys
        let has_signed_key = loaded_manager
            .store
            .get_signed_pre_key(SignedPreKeyId::from(1))
            .await
            .is_ok();
        assert!(
            has_signed_key,
            "Loaded store should have SignedPreKey from previous session"
        );

        // Cleanup
        let _ = super::super::persistence::delete_signal_store(hub_id);
    }
}
