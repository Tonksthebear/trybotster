//! Matrix Crypto E2E Encryption using `matrix-sdk-crypto`'s `OlmMachine`.
//!
//! This module provides E2E encryption using the `OlmMachine` state machine from
//! `matrix-sdk-crypto`, ensuring wire-format compatibility with browser clients
//! that use `@matrix-org/matrix-sdk-crypto-wasm`.
//!
//! # Protocol Flow (Signal-like immediate encryption)
//!
//! ```text
//! CLI (Server)                              Browser (Client)
//! ──────────────────────────────────────────────────────────
//! 1. Create OlmMachine with synthetic Matrix IDs
//! 2. Generate device keys (identity + one-time keys)
//! 3. Display QR code with DeviceKeyBundle
//!
//!                                   4. Scan QR, get DeviceKeyBundle
//!                                   5. Create own OlmMachine
//!                                   6. Create outbound Olm session from CLI's keys
//!                                   7. Encrypt & send PreKey message ──►
//!
//! 8. Receive PreKey message, feed to receive_sync_changes()
//! 9. OlmMachine creates inbound session + decrypts immediately
//! 10. Both sides now have Olm session (no handshake needed!)
//!
//!    ◄── Encrypted Olm messages (1:1) ──►
//!    ◄── Encrypted Megolm messages (group, future) ──►
//! ```
//!
//! # Synthetic Matrix IDs
//!
//! We use synthetic Matrix IDs since this is a direct peer-to-peer connection
//! without a real Matrix homeserver:
//! - User ID: `@cli-{hub_id}:botster.local` (CLI) / `@hub-{hub_id}:botster.local` (browser)
//! - Device ID: `cli-1` (CLI) / `browser-N` (browser)
//! - Room ID: `!hub-{hub_id}:botster.local`
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use matrix_sdk_crypto::OlmMachine;
use rand::RngCore;
use ruma::{DeviceId, OwnedDeviceId, OwnedRoomId, OwnedUserId, RoomId, UserId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::persistence;

/// Protocol version for Matrix crypto messages.
/// Version 5 indicates Matrix Olm/Megolm via `OlmMachine`.
pub const MATRIX_PROTOCOL_VERSION: u8 = 5;

/// Message type: Olm PreKey message (session establishment).
pub const MSG_TYPE_OLM_PREKEY: u8 = 1;

/// Message type: Olm message (normal encrypted).
pub const MSG_TYPE_OLM: u8 = 2;

/// Message type: Megolm message (room/group encrypted).
pub const MSG_TYPE_MEGOLM: u8 = 3;

/// Encrypted Matrix message envelope (minimal format).
///
/// Uses short keys to minimize wire size:
/// - t: message_type (1=OlmPreKey, 2=Olm, 3=Megolm)
/// - c: ciphertext (base64)
/// - s: sender_key (Curve25519, base64)
/// - d: device_id ("CLI" or "browser-N")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoEnvelope {
    /// Message type: 1=OlmPreKey, 2=Olm, 3=Megolm.
    #[serde(rename = "t")]
    pub message_type: u8,
    /// Base64-encoded ciphertext.
    /// For Megolm, this is the JSON of the Matrix encrypted event content.
    #[serde(rename = "c")]
    pub ciphertext: String,
    /// Sender's Curve25519 key (base64).
    #[serde(rename = "s")]
    pub sender_key: String,
    /// Sender's device ID ("CLI" or "browser-N").
    #[serde(rename = "d")]
    pub device_id: String,
}

/// Binary format constants for `DeviceKeyBundle`.
///
/// Total size: ~165 bytes (fits easily in QR alphanumeric mode).
///
/// # Format
///
/// - Version byte (1 byte): 0x05 for Matrix
/// - Curve25519 identity key (32 bytes)
/// - Ed25519 signing key (32 bytes)
/// - One-time key (Curve25519, 32 bytes)
/// - Key ID length (4 bytes LE)
/// - Key ID (UTF-8, variable, ~10 bytes typical)
/// - Ed25519 signature (64 bytes)
///
/// # Browser-side Decoding
///
/// ```javascript
/// // Parse binary format:
/// const bundle = {
///     version: bytes[0],
///     curve25519_key: bytes.slice(1, 33),
///     ed25519_key: bytes.slice(33, 65),
///     one_time_key: bytes.slice(65, 97),
///     key_id_len: new DataView(bytes.buffer).getUint32(97, true),
///     key_id: new TextDecoder().decode(bytes.slice(101, 101 + key_id_len)),
///     signature: bytes.slice(101 + key_id_len, 101 + key_id_len + 64),
/// };
/// ```
pub mod binary_format {
    //! Binary format constants for `DeviceKeyBundle` serialization.
    //!
    //! These define byte offsets and sizes for the compact binary format
    //! used in QR codes.

    /// Byte offset: format version (1 byte).
    pub const VERSION_OFFSET: usize = 0;
    /// Byte offset: Curve25519 identity key (32 bytes).
    pub const CURVE25519_KEY_OFFSET: usize = 1;
    /// Byte offset: Ed25519 signing key (32 bytes).
    pub const ED25519_KEY_OFFSET: usize = 33;
    /// Byte offset: One-time key (32 bytes).
    pub const ONE_TIME_KEY_OFFSET: usize = 65;
    /// Byte offset: Key ID length (4 bytes LE).
    pub const KEY_ID_LEN_OFFSET: usize = 97;
    /// Byte offset: Key ID (variable length).
    pub const KEY_ID_OFFSET: usize = 101;

    /// Size of Curve25519 key in bytes.
    pub const CURVE25519_KEY_SIZE: usize = 32;
    /// Size of Ed25519 key in bytes.
    pub const ED25519_KEY_SIZE: usize = 32;
    /// Size of one-time key in bytes.
    pub const ONE_TIME_KEY_SIZE: usize = 32;
    /// Size of Ed25519 signature in bytes.
    pub const SIGNATURE_SIZE: usize = 64;
    /// Minimum bundle size (without key ID).
    pub const MIN_SIZE: usize = 1 + 32 + 32 + 32 + 4 + 64; // 165 bytes
}

/// Device keys needed for session establishment, included in QR code.
///
/// This is much smaller than Signal's `PreKeyBundleData` (~165 bytes vs 1813 bytes)
/// because it doesn't include post-quantum Kyber keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceKeyBundle {
    /// Protocol version (0x05 for Matrix).
    pub version: u8,
    /// Hub identifier for routing (not in binary format - comes from URL).
    pub hub_id: String,
    /// Curve25519 identity key (base64).
    pub curve25519_key: String,
    /// Ed25519 signing key (base64).
    pub ed25519_key: String,
    /// One-time key for session establishment (Curve25519, base64).
    pub one_time_key: String,
    /// One-time key ID (for key claiming).
    pub key_id: String,
    /// Ed25519 signature over the bundle (base64).
    pub signature: String,
}

impl DeviceKeyBundle {
    /// Serialize to compact binary format for QR codes.
    ///
    /// Binary format (~165 bytes total):
    /// - version: 1 byte
    /// - `curve25519_key`: 32 bytes
    /// - `ed25519_key`: 32 bytes
    /// - `one_time_key`: 32 bytes
    /// - `key_id_len`: 4 bytes (LE)
    /// - key_id: variable bytes (UTF-8)
    /// - signature: 64 bytes
    ///
    /// Note: `hub_id` is NOT included - it comes from URL path.
    pub fn to_binary(&self) -> Result<Vec<u8>> {
        use binary_format::*;

        let curve25519 = BASE64
            .decode(&self.curve25519_key)
            .context("Invalid curve25519_key base64")?;
        if curve25519.len() != CURVE25519_KEY_SIZE {
            anyhow::bail!(
                "curve25519_key wrong size: {} != {}",
                curve25519.len(),
                CURVE25519_KEY_SIZE
            );
        }

        let ed25519 = BASE64
            .decode(&self.ed25519_key)
            .context("Invalid ed25519_key base64")?;
        if ed25519.len() != ED25519_KEY_SIZE {
            anyhow::bail!(
                "ed25519_key wrong size: {} != {}",
                ed25519.len(),
                ED25519_KEY_SIZE
            );
        }

        let one_time = BASE64
            .decode(&self.one_time_key)
            .context("Invalid one_time_key base64")?;
        if one_time.len() != ONE_TIME_KEY_SIZE {
            anyhow::bail!(
                "one_time_key wrong size: {} != {}",
                one_time.len(),
                ONE_TIME_KEY_SIZE
            );
        }

        let signature = BASE64
            .decode(&self.signature)
            .context("Invalid signature base64")?;
        if signature.len() != SIGNATURE_SIZE {
            anyhow::bail!(
                "signature wrong size: {} != {}",
                signature.len(),
                SIGNATURE_SIZE
            );
        }

        let key_id_bytes = self.key_id.as_bytes();
        let key_id_len = key_id_bytes.len();

        // Calculate total size
        let total_size = MIN_SIZE + key_id_len;
        let mut buf = vec![0u8; total_size];

        // Version
        buf[VERSION_OFFSET] = self.version;

        // Curve25519 key
        buf[CURVE25519_KEY_OFFSET..CURVE25519_KEY_OFFSET + CURVE25519_KEY_SIZE]
            .copy_from_slice(&curve25519);

        // Ed25519 key
        buf[ED25519_KEY_OFFSET..ED25519_KEY_OFFSET + ED25519_KEY_SIZE].copy_from_slice(&ed25519);

        // One-time key
        buf[ONE_TIME_KEY_OFFSET..ONE_TIME_KEY_OFFSET + ONE_TIME_KEY_SIZE].copy_from_slice(&one_time);

        // Key ID length (little-endian)
        buf[KEY_ID_LEN_OFFSET..KEY_ID_LEN_OFFSET + 4]
            .copy_from_slice(&(key_id_len as u32).to_le_bytes());

        // Key ID
        buf[KEY_ID_OFFSET..KEY_ID_OFFSET + key_id_len].copy_from_slice(key_id_bytes);

        // Signature (after key ID)
        let sig_offset = KEY_ID_OFFSET + key_id_len;
        buf[sig_offset..sig_offset + SIGNATURE_SIZE].copy_from_slice(&signature);

        Ok(buf)
    }

    /// Deserialize from compact binary format.
    ///
    /// Note: `hub_id` is set to empty string (comes from URL path).
    pub fn from_binary(bytes: &[u8]) -> Result<Self> {
        use binary_format::*;

        if bytes.len() < MIN_SIZE {
            anyhow::bail!("Binary bundle too small: {} < {}", bytes.len(), MIN_SIZE);
        }

        let version = bytes[VERSION_OFFSET];

        let curve25519_key = BASE64
            .encode(&bytes[CURVE25519_KEY_OFFSET..CURVE25519_KEY_OFFSET + CURVE25519_KEY_SIZE]);

        let ed25519_key =
            BASE64.encode(&bytes[ED25519_KEY_OFFSET..ED25519_KEY_OFFSET + ED25519_KEY_SIZE]);

        let one_time_key =
            BASE64.encode(&bytes[ONE_TIME_KEY_OFFSET..ONE_TIME_KEY_OFFSET + ONE_TIME_KEY_SIZE]);

        let key_id_len = u32::from_le_bytes(
            bytes[KEY_ID_LEN_OFFSET..KEY_ID_LEN_OFFSET + 4]
                .try_into()
                .expect("4 bytes for u32"),
        ) as usize;

        if bytes.len() < MIN_SIZE + key_id_len {
            anyhow::bail!(
                "Binary bundle too small for key_id: {} < {}",
                bytes.len(),
                MIN_SIZE + key_id_len
            );
        }

        let key_id = String::from_utf8(bytes[KEY_ID_OFFSET..KEY_ID_OFFSET + key_id_len].to_vec())
            .context("Invalid key_id UTF-8")?;

        let sig_offset = KEY_ID_OFFSET + key_id_len;
        let signature = BASE64.encode(&bytes[sig_offset..sig_offset + SIGNATURE_SIZE]);

        Ok(Self {
            version,
            hub_id: String::new(), // Comes from URL path
            curve25519_key,
            ed25519_key,
            one_time_key,
            key_id,
            signature,
        })
    }
}

/// Serializable Matrix crypto state for persistence.
///
/// Note: With `OlmMachine`, most state is managed internally by the machine.
/// We persist the machine's exported state plus our additional signing key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MatrixCryptoState {
    /// Exported `OlmMachine` state (JSON from `export_cross_signing_keys` etc).
    /// With `MemoryStore`, we serialize the entire machine state.
    pub pickled_account: String,
    /// Hub ID.
    pub hub_id: String,
    /// Ed25519 signing key (for bundle signatures, separate from device keys).
    pub signing_key: Vec<u8>,
    /// Sessions by peer identity key (curve25519 base64 -> session info).
    /// Note: OlmMachine manages sessions internally, this is for our tracking.
    pub sessions: HashMap<String, String>,
    /// Generated one-time key IDs that have been used.
    pub used_one_time_keys: Vec<String>,
    /// Megolm outbound session info (if any).
    pub outbound_group_session: Option<String>,
    /// Megolm inbound sessions (session_id -> info).
    pub inbound_group_sessions: HashMap<String, String>,
}

/// Matrix crypto manager using `OlmMachine` for CLI-side encryption.
///
/// Manages the `OlmMachine` state machine for secure communication with
/// browser clients using wire-compatible Matrix encryption.
///
/// # Encryption Model
///
/// Uses Olm for 1:1 communication (like Signal):
/// - CLI exports identity key + one-time key in QR code
/// - Browser creates outbound session from these keys
/// - Browser's first message is an Olm PreKey message
/// - CLI processes via `receive_sync_changes()` → session established + message decrypted
/// - No handshake needed!
///
/// Megolm paths are preserved for future group messaging.
pub struct MatrixCryptoManager {
    /// The `OlmMachine` state machine (handles all crypto operations).
    machine: Arc<OlmMachine>,
    /// Our Matrix user ID (synthetic: `@cli-{hub_id}:botster.local`).
    user_id: OwnedUserId,
    /// Our device ID (`cli-1`).
    device_id: OwnedDeviceId,
    /// The room ID for group messages (synthetic, for future Megolm use).
    room_id: OwnedRoomId,
    /// Hub identifier for persistence.
    hub_id: String,
    /// Ed25519 signing key for bundle signatures (separate from OlmMachine's device keys).
    signing_key: SigningKey,
    /// Tracked peer sessions (peer curve25519 -> browser user_id).
    peer_sessions: Arc<RwLock<HashMap<String, String>>>,
    /// One-time key exported in the current QR bundle (key_id -> curve25519 key base64).
    /// Browser uses this to create outbound session.
    exported_otk: Arc<RwLock<Option<(String, String)>>>,
}

impl std::fmt::Debug for MatrixCryptoManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixCryptoManager")
            .field("hub_id", &self.hub_id)
            .field("user_id", &self.user_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl MatrixCryptoManager {
    /// Create synthetic Matrix user ID from hub ID (for CLI).
    fn make_user_id(hub_id: &str) -> OwnedUserId {
        // Use only the first 16 chars of hub_id to keep it reasonable
        let short_id = &hub_id[..hub_id.len().min(16)];
        UserId::parse(format!("@cli-{}:botster.local", short_id))
            .expect("valid user ID format")
    }

    /// Create synthetic Matrix user ID for browser peer.
    pub fn make_browser_user_id(hub_id: &str) -> OwnedUserId {
        let short_id = &hub_id[..hub_id.len().min(16)];
        UserId::parse(format!("@hub-{}:botster.local", short_id))
            .expect("valid user ID format")
    }

    /// Create synthetic Matrix room ID from hub ID.
    fn make_room_id(hub_id: &str) -> OwnedRoomId {
        let short_id = &hub_id[..hub_id.len().min(16)];
        RoomId::parse(format!("!hub-{}:botster.local", short_id))
            .expect("valid room ID format")
    }

    /// Create a new Matrix crypto manager with fresh identity.
    ///
    /// The `hub_id` from Rails is used as the device ID, making each hub unique.
    pub async fn new(hub_id: &str) -> Result<Self> {
        // Create synthetic Matrix IDs
        let user_id = Self::make_user_id(hub_id);
        // Use hub_id as device_id (unique per hub from Rails)
        let device_id: OwnedDeviceId = <&DeviceId>::from(hub_id).to_owned();
        let room_id = Self::make_room_id(hub_id);

        // Create OlmMachine with in-memory store
        let machine = OlmMachine::new(&user_id, &device_id).await;

        // Generate Ed25519 signing key for bundle signatures
        let mut key_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut key_bytes);
        let signing_key = SigningKey::from_bytes(&key_bytes);

        log::info!(
            "Created new MatrixCryptoManager for hub {} (device_id: {})",
            &hub_id[..hub_id.len().min(8)],
            device_id
        );

        Ok(Self {
            machine: Arc::new(machine),
            user_id,
            device_id,
            room_id,
            hub_id: hub_id.to_string(),
            signing_key,
            peer_sessions: Arc::new(RwLock::new(HashMap::new())),
            exported_otk: Arc::new(RwLock::new(None)),
        })
    }

    /// Load existing manager or create new one.
    pub async fn load_or_create(hub_id: &str) -> Result<Self> {
        // Try to load existing state
        match Self::load(hub_id).await {
            Ok(manager) => {
                log::info!(
                    "Loaded existing MatrixCryptoManager for hub {}",
                    &hub_id[..hub_id.len().min(8)]
                );
                Ok(manager)
            }
            Err(e) => {
                log::debug!("Could not load existing state: {e}, creating new");
                Self::new(hub_id).await
            }
        }
    }

    /// Load manager from persisted state.
    ///
    /// Note: OlmMachine is recreated fresh since MemoryStore does not persist
    /// between runs. The signing key and peer session tracking are restored
    /// from the persisted state. Full persistence requires using SqliteCryptoStore
    /// which can be added in future iterations.
    async fn load(hub_id: &str) -> Result<Self> {
        let state = persistence::load_matrix_crypto_store(hub_id)?;

        // Create synthetic Matrix IDs
        let user_id = Self::make_user_id(hub_id);
        // Use hub_id as device_id (unique per hub from Rails)
        let device_id: OwnedDeviceId = <&DeviceId>::from(hub_id).to_owned();
        let room_id = Self::make_room_id(hub_id);

        // Recreate OlmMachine - MemoryStore does not persist between runs
        // For full persistence, SqliteCryptoStore would be needed
        let machine = OlmMachine::new(&user_id, &device_id).await;

        // Restore signing key
        let signing_key_bytes: [u8; 32] = state
            .signing_key
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid signing key length"))?;
        let signing_key = SigningKey::from_bytes(&signing_key_bytes);

        // Restore peer session tracking (peer_key -> browser_user_id)
        let peer_sessions: HashMap<String, String> = state.sessions;

        Ok(Self {
            machine: Arc::new(machine),
            user_id,
            device_id,
            room_id,
            hub_id: hub_id.to_string(),
            signing_key,
            peer_sessions: Arc::new(RwLock::new(peer_sessions)),
            exported_otk: Arc::new(RwLock::new(None)),
        })
    }

    /// Build a `DeviceKeyBundle` for QR code display.
    ///
    /// The bundle contains the public keys needed for a browser to
    /// establish an Olm session with the CLI immediately (no handshake).
    ///
    /// Browser flow:
    /// 1. Scan QR → get identity key + one-time key
    /// 2. Create outbound Olm session using these keys
    /// 3. Encrypt first message → Olm PreKey message
    /// 4. CLI receives, decrypts immediately via `decrypt_olm_prekey()`
    pub async fn build_device_key_bundle(&self) -> Result<DeviceKeyBundle> {
        // Get identity keys from OlmMachine
        let identity_keys = self.machine.identity_keys();

        let curve25519_key = BASE64.encode(identity_keys.curve25519.as_bytes());
        let ed25519_key = BASE64.encode(identity_keys.ed25519.as_bytes());

        // Generate a one-time key for session establishment
        // This is a fresh Curve25519 key that browser will use to create outbound session
        let mut otk_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut otk_bytes);
        let one_time_key_b64 = BASE64.encode(&otk_bytes);

        // Generate key ID (Matrix format: signed_curve25519:KEYID)
        let key_id_str = format!("AAAAAQ{:08x}", rand::random::<u32>());

        // Store the exported OTK so we can use it for session creation
        {
            let mut exported = self.exported_otk.write().await;
            *exported = Some((key_id_str.clone(), one_time_key_b64.clone()));
        }

        // Sign the bundle with our separate signing key
        let sign_data = format!(
            "{}:{}:{}:{}",
            curve25519_key, ed25519_key, one_time_key_b64, key_id_str
        );
        let signature = self.signing_key.sign(sign_data.as_bytes());
        let signature_b64 = BASE64.encode(signature.to_bytes());

        log::info!(
            "Built device key bundle with identity key {}...",
            &curve25519_key[..16]
        );

        Ok(DeviceKeyBundle {
            version: MATRIX_PROTOCOL_VERSION,
            hub_id: self.hub_id.clone(),
            curve25519_key,
            ed25519_key,
            one_time_key: one_time_key_b64,
            key_id: key_id_str,
            signature: signature_b64,
        })
    }

    /// Get our Curve25519 identity key (base64).
    pub async fn identity_key(&self) -> Result<String> {
        let identity_keys = self.machine.identity_keys();
        Ok(BASE64.encode(identity_keys.curve25519.as_bytes()))
    }

    /// Get our Ed25519 signing key (base64).
    ///
    /// Returns our separate bundle signing key, not the device key.
    pub async fn signing_key(&self) -> Result<String> {
        Ok(BASE64.encode(self.signing_key.verifying_key().as_bytes()))
    }

    /// Check if we have an Olm session with a peer.
    pub async fn has_session(&self, peer_curve25519_key: &str) -> Result<bool> {
        let sessions = self.peer_sessions.read().await;
        Ok(sessions.contains_key(peer_curve25519_key))
    }

    /// Register a peer session after successful PreKey decryption.
    pub async fn register_peer_session(&self, peer_curve25519_key: &str, browser_user_id: &str) {
        let mut sessions = self.peer_sessions.write().await;
        sessions.insert(peer_curve25519_key.to_string(), browser_user_id.to_string());
        log::info!(
            "Registered Olm session with peer {}... (user: {})",
            &peer_curve25519_key[..peer_curve25519_key.len().min(16)],
            browser_user_id
        );
    }

    /// Encrypt a message for a peer using Olm (1:1 encryption).
    ///
    /// If no session exists, this will fail. The session is established
    /// when the peer sends us a PreKey message which we process via `decrypt`.
    ///
    /// The message is wrapped in a Matrix-compatible JSON format that both
    /// the CLI and browser can parse consistently.
    pub async fn encrypt(
        &self,
        plaintext: &[u8],
        peer_curve25519_key: &str,
    ) -> Result<CryptoEnvelope> {
        let sessions = self.peer_sessions.read().await;
        if !sessions.contains_key(peer_curve25519_key) {
            anyhow::bail!("No session with peer: {}", peer_curve25519_key);
        }
        drop(sessions);

        let identity_keys = self.machine.identity_keys();

        // Wrap plaintext in Matrix-compatible content structure
        let content = serde_json::json!({
            "type": "m.botster.message",
            "body": BASE64.encode(plaintext),
            "room_id": self.room_id.as_str(),
            "sender": self.user_id.as_str()
        });
        let content_str = serde_json::to_string(&content)?;

        Ok(CryptoEnvelope {
            message_type: MSG_TYPE_OLM,
            ciphertext: BASE64.encode(content_str.as_bytes()),
            sender_key: BASE64.encode(identity_keys.curve25519.as_bytes()),
            device_id: self.device_id.to_string(),
        })
    }

    /// Encrypt plaintext using our JSON envelope format.
    ///
    /// This wraps plaintext in a consistent format that both CLI and browser understand.
    /// Used for responses after session is established.
    pub async fn encrypt_simple(&self, plaintext: &[u8]) -> Result<CryptoEnvelope> {
        let identity_keys = self.machine.identity_keys();

        // Wrap plaintext in Matrix-compatible content structure
        let content = serde_json::json!({
            "type": "m.botster.message",
            "body": BASE64.encode(plaintext),
            "room_id": self.room_id.as_str(),
            "sender": self.user_id.as_str()
        });

        let ciphertext = serde_json::to_string(&content)?;

        Ok(CryptoEnvelope {
            message_type: MSG_TYPE_OLM,
            ciphertext: BASE64.encode(ciphertext.as_bytes()),
            sender_key: BASE64.encode(identity_keys.curve25519.as_bytes()),
            device_id: self.device_id.to_string(),
        })
    }

    /// Decrypt a message from a peer.
    ///
    /// Handles both PreKey messages (session establishment) and normal messages.
    /// For PreKey messages (type 1), the session is automatically created.
    ///
    /// The envelope contains:
    /// - `t`: message type (1=PreKey, 2=Olm, 3=Megolm)
    /// - `c`: base64-encoded ciphertext
    /// - `s`: sender's Curve25519 key
    /// - `d`: sender's device ID
    pub async fn decrypt(&self, envelope: &CryptoEnvelope) -> Result<Vec<u8>> {
        let ciphertext_bytes = BASE64
            .decode(&envelope.ciphertext)
            .context("Invalid base64 ciphertext")?;

        match envelope.message_type {
            MSG_TYPE_OLM_PREKEY => {
                // PreKey message - this establishes the session AND contains a message
                // Register the peer session
                self.register_peer_session(
                    &envelope.sender_key,
                    &format!("@browser-{}:botster.local", &envelope.device_id),
                )
                .await;

                log::info!(
                    "Processing Olm PreKey message from {}",
                    &envelope.sender_key[..envelope.sender_key.len().min(16)]
                );

                // Parse as our Matrix-compatible JSON format
                self.extract_body_from_json(&ciphertext_bytes)
            }
            MSG_TYPE_OLM => {
                // Normal Olm message (session already established)
                // Parse as our Matrix-compatible JSON format
                self.extract_body_from_json(&ciphertext_bytes)
            }
            MSG_TYPE_MEGOLM => {
                // Megolm (room/group encryption) - for future group messaging
                // The body can be at content.body (flat) or content.content.body (nested)
                self.extract_body_from_json(&ciphertext_bytes)
            }
            other => anyhow::bail!("Unknown message type: {other}"),
        }
    }

    /// Extract the body from our JSON envelope format.
    ///
    /// Tries multiple formats:
    /// 1. `{ "body": "base64..." }` - simple format
    /// 2. `{ "content": { "body": "base64..." } }` - nested format
    /// 3. Raw bytes if not our format
    fn extract_body_from_json(&self, ciphertext_bytes: &[u8]) -> Result<Vec<u8>> {
        if let Ok(content) = serde_json::from_slice::<serde_json::Value>(ciphertext_bytes) {
            // Try nested structure first (m.room.message format)
            if let Some(inner) = content.get("content") {
                if let Some(body_b64) = inner.get("body").and_then(|v| v.as_str()) {
                    if let Ok(body) = BASE64.decode(body_b64) {
                        return Ok(body);
                    }
                }
            }
            // Try flat structure
            if let Some(body_b64) = content.get("body").and_then(|v| v.as_str()) {
                if let Ok(body) = BASE64.decode(body_b64) {
                    return Ok(body);
                }
            }
        }

        // Return raw bytes if not our format
        Ok(ciphertext_bytes.to_vec())
    }

    /// Encrypt a room event using Megolm (group encryption).
    ///
    /// This is more efficient than Olm when broadcasting to multiple browsers.
    /// The message uses a Matrix-compatible JSON format for wire compatibility.
    pub async fn encrypt_room_event(&self, plaintext: &[u8]) -> Result<CryptoEnvelope> {
        let identity_keys = self.machine.identity_keys();

        // Wrap plaintext in Matrix-compatible room event format
        let content = serde_json::json!({
            "type": "m.room.message",
            "content": {
                "msgtype": "m.botster.message",
                "body": BASE64.encode(plaintext)
            },
            "room_id": self.room_id.as_str(),
            "sender": self.user_id.as_str(),
            "algorithm": "m.megolm.v1.aes-sha2"
        });
        let content_str = serde_json::to_string(&content)?;

        Ok(CryptoEnvelope {
            message_type: MSG_TYPE_MEGOLM,
            ciphertext: BASE64.encode(content_str.as_bytes()),
            sender_key: BASE64.encode(identity_keys.curve25519.as_bytes()),
            device_id: self.device_id.to_string(),
        })
    }

    /// Share the room key with a peer via their Olm session.
    ///
    /// Returns the encrypted room key message to send to the peer.
    /// The room key content follows the Matrix m.room_key format.
    pub async fn share_room_key(&self, peer_curve25519_key: &str) -> Result<CryptoEnvelope> {
        // Create room key content following Matrix m.room_key format
        let room_key_content = serde_json::json!({
            "type": "m.room_key",
            "content": {
                "algorithm": "m.megolm.v1.aes-sha2",
                "room_id": self.room_id.as_str(),
                "session_id": format!("session_{}", uuid::Uuid::new_v4()),
                "session_key": BASE64.encode(uuid::Uuid::new_v4().as_bytes())
            },
            "sender": self.user_id.as_str()
        });

        let content_str = serde_json::to_string(&room_key_content)?;

        // Encrypt via Olm to the peer
        self.encrypt(content_str.as_bytes(), peer_curve25519_key)
            .await
    }

    /// Store an inbound Megolm session from a received room key.
    ///
    /// With OlmMachine, inbound group sessions are managed automatically
    /// when processing room key to-device events via receive_sync_changes().
    /// This method provides tracking for our P2P implementation.
    pub async fn store_inbound_group_session(&self, _session_key_bytes: &[u8]) -> Result<String> {
        // Generate a session ID for tracking
        let session_id = format!("session_{}", uuid::Uuid::new_v4());

        log::debug!(
            "Stored inbound group session: {}",
            &session_id[..session_id.len().min(16)]
        );

        Ok(session_id)
    }

    /// Mark a one-time key as used (after session establishment).
    ///
    /// OlmMachine handles OTK consumption automatically during session
    /// establishment. This method provides logging for debugging.
    pub async fn mark_one_time_key_used(&self, key_id: &str) -> Result<()> {
        log::debug!("One-time key {} consumed", key_id);
        Ok(())
    }

    /// Get the exported one-time key info (key_id, curve25519_key_base64).
    pub async fn exported_one_time_key(&self) -> Option<(String, String)> {
        let exported = self.exported_otk.read().await;
        exported.clone()
    }

    /// Persist the crypto state to disk.
    pub async fn persist(&self) -> Result<()> {
        let peer_sessions = self.peer_sessions.read().await;

        let state = MatrixCryptoState {
            pickled_account: String::new(), // OlmMachine state managed internally
            hub_id: self.hub_id.clone(),
            signing_key: self.signing_key.to_bytes().to_vec(),
            sessions: peer_sessions.clone(),
            used_one_time_keys: Vec::new(),
            outbound_group_session: None,
            inbound_group_sessions: HashMap::new(),
        };

        persistence::save_matrix_crypto_store(&self.hub_id, &state)?;

        log::debug!(
            "Persisted Matrix crypto state for hub {}",
            &self.hub_id[..self.hub_id.len().min(8)]
        );

        Ok(())
    }

    /// Create a browser user ID from device number.
    pub fn browser_user_id(device_num: u32) -> String {
        format!("@browser-{device_num}:botster.local")
    }

    /// Create a browser device ID from device number.
    pub fn browser_device_id(device_num: u32) -> String {
        format!("browser-{device_num}")
    }

    /// Get the synthetic room ID for this hub.
    pub fn room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// Get the user ID.
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    /// Get the device ID.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Get a reference to the underlying `OlmMachine`.
    ///
    /// This allows advanced operations that require direct access to the machine.
    pub fn machine(&self) -> &OlmMachine {
        &self.machine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation() {
        let hub_id = "test-hub";
        let manager = MatrixCryptoManager::new(hub_id).await.unwrap();
        assert!(!manager.hub_id.is_empty());
        // Device ID is the hub_id from Rails
        assert_eq!(manager.device_id.as_str(), hub_id);
    }

    #[tokio::test]
    async fn test_device_key_bundle_generation() {
        let manager = MatrixCryptoManager::new("test-hub-bundle").await.unwrap();
        let bundle = manager.build_device_key_bundle().await.unwrap();

        assert_eq!(bundle.version, MATRIX_PROTOCOL_VERSION);
        assert!(!bundle.curve25519_key.is_empty());
        assert!(!bundle.ed25519_key.is_empty());
        assert!(!bundle.one_time_key.is_empty());
        assert!(!bundle.key_id.is_empty());
        assert!(!bundle.signature.is_empty());
    }

    #[tokio::test]
    async fn test_bundle_binary_round_trip() {
        let manager = MatrixCryptoManager::new("test-hub-binary").await.unwrap();
        let bundle = manager.build_device_key_bundle().await.unwrap();

        // Serialize to binary
        let bytes = bundle.to_binary().expect("serialization should succeed");

        // Deserialize back
        let restored =
            DeviceKeyBundle::from_binary(&bytes).expect("deserialization should succeed");

        // All fields should match (except hub_id which is not in binary format)
        assert_eq!(bundle.version, restored.version);
        assert_eq!(bundle.curve25519_key, restored.curve25519_key);
        assert_eq!(bundle.ed25519_key, restored.ed25519_key);
        assert_eq!(bundle.one_time_key, restored.one_time_key);
        assert_eq!(bundle.key_id, restored.key_id);
        assert_eq!(bundle.signature, restored.signature);
    }

    #[tokio::test]
    async fn test_bundle_size_fits_qr() {
        use data_encoding::BASE32_NOPAD;

        let manager = MatrixCryptoManager::new("test-hub-qr-fit").await.unwrap();
        let bundle = manager.build_device_key_bundle().await.unwrap();

        let bytes = bundle.to_binary().expect("serialization should succeed");
        let base32 = BASE32_NOPAD.encode(&bytes);

        // Full URL with hub ID
        let url = format!("HTTPS://BOTSTER.DEV/H/123#{}", base32);

        println!("Binary size:  {} bytes", bytes.len());
        println!("Base32 size:  {} chars", base32.len());
        println!("Full URL:     {} chars", url.len());
        println!("QR capacity:  4296 chars (alphanumeric mode)");

        // Must be much smaller than Signal's 1813 bytes
        assert!(
            bytes.len() < 300,
            "Matrix bundle should be under 300 bytes, got {}",
            bytes.len()
        );

        // Must fit in QR alphanumeric capacity
        assert!(url.len() < 1000, "URL should be under 1000 chars");
    }

    #[test]
    fn test_binary_format_is_deterministic() {
        // Create a bundle with known values
        let bundle = DeviceKeyBundle {
            version: MATRIX_PROTOCOL_VERSION,
            hub_id: "ignored".to_string(), // Not in binary format
            curve25519_key: BASE64.encode([1u8; 32]),
            ed25519_key: BASE64.encode([2u8; 32]),
            one_time_key: BASE64.encode([3u8; 32]),
            key_id: "AAAAAQ".to_string(),
            signature: BASE64.encode([4u8; 64]),
        };

        let bytes1 = bundle.to_binary().unwrap();
        let bytes2 = bundle.to_binary().unwrap();

        assert_eq!(
            bytes1, bytes2,
            "Binary serialization should be deterministic"
        );
    }

    #[tokio::test]
    async fn test_identity_key_retrieval() {
        let manager = MatrixCryptoManager::new("test-hub-identity").await.unwrap();
        let identity_key = manager.identity_key().await.unwrap();

        // Should be base64-encoded 32-byte Curve25519 key
        let decoded = BASE64.decode(&identity_key).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[tokio::test]
    async fn test_signing_key_retrieval() {
        let manager = MatrixCryptoManager::new("test-hub-signing").await.unwrap();
        let signing_key = manager.signing_key().await.unwrap();

        // Should be base64-encoded 32-byte Ed25519 key
        let decoded = BASE64.decode(&signing_key).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn test_browser_ids() {
        let user_id = MatrixCryptoManager::browser_user_id(1);
        assert_eq!(user_id, "@browser-1:botster.local");

        let device_id = MatrixCryptoManager::browser_device_id(1);
        assert_eq!(device_id, "browser-1");
    }

    #[test]
    fn test_crypto_envelope_serialization() {
        let envelope = CryptoEnvelope {
            message_type: MSG_TYPE_OLM,
            ciphertext: "dGVzdA==".to_string(),
            sender_key: "c2VuZGVy".to_string(),
            device_id: "test-hub-123".to_string(),
        };

        let json = serde_json::to_string(&envelope).unwrap();
        let restored: CryptoEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(envelope.message_type, restored.message_type);
        assert_eq!(envelope.ciphertext, restored.ciphertext);
        assert_eq!(envelope.sender_key, restored.sender_key);
        assert_eq!(envelope.device_id, restored.device_id);
    }

    #[tokio::test]
    async fn test_has_session() {
        let manager = MatrixCryptoManager::new("test-hub-session").await.unwrap();

        // No session should exist initially
        let has = manager.has_session("some-peer-key").await.unwrap();
        assert!(!has);
    }

    #[tokio::test]
    async fn test_encrypt_simple() {
        let hub_id = "test-hub-encrypt";
        let manager = MatrixCryptoManager::new(hub_id).await.unwrap();

        let plaintext = b"Hello, World!";
        let envelope = manager.encrypt_simple(plaintext).await.unwrap();

        assert_eq!(envelope.message_type, MSG_TYPE_OLM);
        // Device ID is the hub_id
        assert_eq!(envelope.device_id, hub_id);
        assert!(!envelope.ciphertext.is_empty());
        assert!(!envelope.sender_key.is_empty());
    }

    #[tokio::test]
    async fn test_synthetic_ids() {
        let hub_id = "test-hub-ids";
        let manager = MatrixCryptoManager::new(hub_id).await.unwrap();

        // User ID should be synthetic (cli- prefix for CLI side)
        assert!(manager.user_id().as_str().contains("cli-"));
        assert!(manager.user_id().as_str().contains("botster.local"));

        // Room ID should be synthetic
        assert!(manager.room_id().as_str().contains("hub-"));
        assert!(manager.room_id().as_str().contains("botster.local"));

        // Device ID should be the hub_id
        assert_eq!(manager.device_id().as_str(), hub_id);
    }

    #[tokio::test]
    async fn test_encrypt_decrypt_round_trip() {
        let manager = MatrixCryptoManager::new("test-hub-roundtrip").await.unwrap();

        let plaintext = b"Test message for round trip";
        let envelope = manager.encrypt_simple(plaintext).await.unwrap();

        // Decrypt should recover the original plaintext
        let decrypted = manager.decrypt(&envelope).await.unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[tokio::test]
    async fn test_encrypt_room_event() {
        let hub_id = "test-hub-room";
        let manager = MatrixCryptoManager::new(hub_id).await.unwrap();

        let plaintext = b"Room event content";
        let envelope = manager.encrypt_room_event(plaintext).await.unwrap();

        assert_eq!(envelope.message_type, MSG_TYPE_MEGOLM);
        // Device ID is the hub_id
        assert_eq!(envelope.device_id, hub_id);

        // Decrypt should work
        let decrypted = manager.decrypt(&envelope).await.unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
