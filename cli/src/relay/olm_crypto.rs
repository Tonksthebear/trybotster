//! E2E encryption using vodozemac (direct Olm, no matrix-sdk-crypto).
//!
//! This module provides E2E encryption for the CLI using vodozemac directly,
//! replacing the heavyweight `matrix-sdk-crypto` OlmMachine. The result is
//! simpler, synchronous, and ~400 lines instead of ~1800.
//!
//! # Dual Encryption Paths
//!
//! Two paths exist because ActionCable (JSON) and DataChannel (binary) have
//! different wire requirements. The Olm encrypt/decrypt is identical — only
//! the serialization differs.
//!
//! | Path | Wire format | Used for |
//! |------|-------------|----------|
//! | `encrypt()` / `decrypt()` | `OlmEnvelope` JSON (`{t, b, k?}`) | ActionCable signaling (SDP, ICE) |
//! | `encrypt_binary()` / `decrypt_binary()` | `[type:1][key?:32][ciphertext]` | DataChannel messages |
//!
//! # Binary Inner Content Format (DataChannel)
//!
//! After Olm decryption, the first byte indicates content type:
//! - `0x00` (CONTENT_MSG): `[0x00][JSON bytes]` — control messages
//! - `0x01` (CONTENT_PTY): `[0x01][flags:1][sub_id_len:1][sub_id][payload]` — PTY I/O
//!   flags bit 0: compressed (gzip), bit 1: input direction (browser→CLI)
//!
//! # Protocol Flow
//!
//! ```text
//! CLI (Server)                              Browser (Client)
//! ──────────────────────────────────────────────────────────
//! 1. Create vodozemac Account
//! 2. Generate device keys (identity + one-time key)
//! 3. Display QR code with DeviceKeyBundle (v6)
//!
//!                                   4. Scan QR, get DeviceKeyBundle
//!                                   5. Create own vodozemac Account
//!                                   6. Create outbound Olm session from CLI's keys
//!                                   7. Encrypt & send PreKey message via DataChannel
//!
//! 8. Receive PreKey message
//! 9. create_inbound_session() → session + plaintext
//! 10. Both sides have Olm session
//!
//!    ◄── Encrypted OlmEnvelope messages ──►
//! ```

use std::collections::HashMap;

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use vodozemac::olm::{Account, OlmMessage, Session};
#[cfg(test)]
use vodozemac::olm::SessionConfig;
use vodozemac::Curve25519PublicKey;

use super::persistence;

/// Extract the Olm identity key from a browser_identity string.
///
/// Browser identity format: `{olm_identity_key}:{tab_id}`.
/// Returns the identity key portion (before first colon), or the
/// full string if no colon is present.
pub fn extract_olm_key(browser_identity: &str) -> &str {
    browser_identity.split(':').next().unwrap_or(browser_identity)
}

/// Decode base64 that may or may not have padding.
fn decode_b64(input: &str) -> Result<Vec<u8>> {
    STANDARD_NO_PAD
        .decode(input)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD
                .decode(input)
        })
        .context("Invalid base64")
}

/// Protocol version for vodozemac crypto messages.
/// Version 6 indicates direct vodozemac (no matrix-sdk-crypto wrapper).
pub const PROTOCOL_VERSION: u8 = 6;

/// Olm PreKey message type (session establishment).
pub const MSG_TYPE_PREKEY: u8 = 0;

/// Olm normal message type.
pub const MSG_TYPE_NORMAL: u8 = 1;

/// Bundle refresh message type (CLI → Browser, unencrypted).
///
/// Sent when the CLI detects session desync (consecutive decrypt failures).
/// Contains a fresh `DeviceKeyBundle` (161 bytes) so the browser can
/// re-establish the Olm session without rescanning the QR code.
pub const MSG_TYPE_BUNDLE_REFRESH: u8 = 2;

/// Binary inner content type: JSON control message.
pub const CONTENT_MSG: u8 = 0x00;

/// Binary inner content type: PTY output.
pub const CONTENT_PTY: u8 = 0x01;

/// Binary inner content type: TCP stream multiplexer.
pub const CONTENT_STREAM: u8 = 0x02;

/// Binary inner content type: file transfer (browser → CLI).
pub const CONTENT_FILE: u8 = 0x03;

/// Encrypted message envelope (minimal wire format).
///
/// Uses short keys to minimize wire size:
/// - `t`: message type (0=PreKey, 1=Normal)
/// - `b`: ciphertext (base64 unpadded)
/// - `k`: sender's Curve25519 identity key (base64, only on PreKey)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OlmEnvelope {
    /// Message type: 0=PreKey, 1=Normal.
    #[serde(rename = "t")]
    pub message_type: u8,
    /// Base64-unpadded ciphertext.
    #[serde(rename = "b")]
    pub ciphertext: String,
    /// Sender's Curve25519 identity key (base64 unpadded).
    /// Present on PreKey messages for session establishment.
    #[serde(rename = "k", skip_serializing_if = "Option::is_none")]
    pub sender_key: Option<String>,
}

/// Binary format constants for `DeviceKeyBundle`.
///
/// Fixed-size format (161 bytes):
/// - Version byte (1 byte): 0x06
/// - Curve25519 identity key (32 bytes)
/// - Ed25519 signing key (32 bytes)
/// - One-time key (Curve25519, 32 bytes)
/// - Ed25519 signature (64 bytes)
///
/// Total: 1 + 32 + 32 + 32 + 64 = 161 bytes.
pub mod binary_format {
    //! Binary format constants for `DeviceKeyBundle` serialization.

    /// Byte offset: format version (1 byte).
    pub const VERSION_OFFSET: usize = 0;
    /// Byte offset: Curve25519 identity key (32 bytes).
    pub const CURVE25519_KEY_OFFSET: usize = 1;
    /// Byte offset: Ed25519 signing key (32 bytes).
    pub const ED25519_KEY_OFFSET: usize = 33;
    /// Byte offset: One-time key (32 bytes).
    pub const ONE_TIME_KEY_OFFSET: usize = 65;
    /// Byte offset: Ed25519 signature (64 bytes).
    pub const SIGNATURE_OFFSET: usize = 97;

    /// Size of Curve25519 key in bytes.
    pub const CURVE25519_KEY_SIZE: usize = 32;
    /// Size of Ed25519 key in bytes.
    pub const ED25519_KEY_SIZE: usize = 32;
    /// Size of one-time key in bytes.
    pub const ONE_TIME_KEY_SIZE: usize = 32;
    /// Size of Ed25519 signature in bytes.
    pub const SIGNATURE_SIZE: usize = 64;
    /// Total fixed bundle size.
    pub const BUNDLE_SIZE: usize = 1 + 32 + 32 + 32 + 64; // 161 bytes
}

/// Device keys for session establishment, included in QR code.
///
/// v6 format: fixed 161 bytes binary, no variable-length fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceKeyBundle {
    /// Protocol version (0x06 for vodozemac).
    pub version: u8,
    /// Hub identifier for routing (not in binary format -- comes from URL).
    pub hub_id: String,
    /// Curve25519 identity key (base64 unpadded).
    pub curve25519_key: String,
    /// Ed25519 signing key (base64 unpadded).
    pub ed25519_key: String,
    /// One-time key for session establishment (Curve25519, base64 unpadded).
    pub one_time_key: String,
    /// Ed25519 signature over bytes[0..97] (base64 unpadded).
    pub signature: String,
}

impl DeviceKeyBundle {
    /// Serialize to compact binary format for QR codes.
    ///
    /// Fixed-size 161 bytes: `[1 version][32 identity][32 signing][32 otk][64 signature]`.
    /// `hub_id` is NOT included -- it comes from the URL path.
    pub fn to_binary(&self) -> Result<Vec<u8>> {
        use binary_format::*;

        let curve25519 = decode_b64(&self.curve25519_key)?;
        anyhow::ensure!(curve25519.len() == CURVE25519_KEY_SIZE, "curve25519_key wrong size");

        let ed25519 = decode_b64(&self.ed25519_key)?;
        anyhow::ensure!(ed25519.len() == ED25519_KEY_SIZE, "ed25519_key wrong size");

        let one_time = decode_b64(&self.one_time_key)?;
        anyhow::ensure!(one_time.len() == ONE_TIME_KEY_SIZE, "one_time_key wrong size");

        let signature = decode_b64(&self.signature)?;
        anyhow::ensure!(signature.len() == SIGNATURE_SIZE, "signature wrong size");

        let mut buf = vec![0u8; BUNDLE_SIZE];
        buf[VERSION_OFFSET] = self.version;
        buf[CURVE25519_KEY_OFFSET..CURVE25519_KEY_OFFSET + CURVE25519_KEY_SIZE]
            .copy_from_slice(&curve25519);
        buf[ED25519_KEY_OFFSET..ED25519_KEY_OFFSET + ED25519_KEY_SIZE]
            .copy_from_slice(&ed25519);
        buf[ONE_TIME_KEY_OFFSET..ONE_TIME_KEY_OFFSET + ONE_TIME_KEY_SIZE]
            .copy_from_slice(&one_time);
        buf[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_SIZE]
            .copy_from_slice(&signature);

        Ok(buf)
    }

    /// Deserialize from compact binary format.
    ///
    /// `hub_id` is set to empty string (comes from URL path).
    pub fn from_binary(bytes: &[u8]) -> Result<Self> {
        use binary_format::*;

        anyhow::ensure!(bytes.len() >= BUNDLE_SIZE, "Binary bundle too small: {} < {}", bytes.len(), BUNDLE_SIZE);

        let version = bytes[VERSION_OFFSET];

        let curve25519_key = STANDARD_NO_PAD
            .encode(&bytes[CURVE25519_KEY_OFFSET..CURVE25519_KEY_OFFSET + CURVE25519_KEY_SIZE]);
        let ed25519_key = STANDARD_NO_PAD
            .encode(&bytes[ED25519_KEY_OFFSET..ED25519_KEY_OFFSET + ED25519_KEY_SIZE]);
        let one_time_key = STANDARD_NO_PAD
            .encode(&bytes[ONE_TIME_KEY_OFFSET..ONE_TIME_KEY_OFFSET + ONE_TIME_KEY_SIZE]);
        let signature = STANDARD_NO_PAD
            .encode(&bytes[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_SIZE]);

        Ok(Self {
            version,
            hub_id: String::new(),
            curve25519_key,
            ed25519_key,
            one_time_key,
            signature,
        })
    }
}

/// Serializable vodozemac crypto state for persistence.
///
/// Supports multiple concurrent Olm sessions (one per browser device).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VodozemacCryptoState {
    /// Pickled Account (vodozemac's serialized format).
    pub pickled_account: String,
    /// Hub ID.
    pub hub_id: String,
    /// Pickled sessions keyed by peer identity key (Curve25519, base64).
    #[serde(default)]
    pub pickled_sessions: HashMap<String, String>,
}

/// Vodozemac crypto manager for CLI-side encryption.
///
/// Manages a vodozemac `Account` and multiple `Session`s for secure
/// communication with browser devices. Each browser device gets its own
/// Olm session, keyed by the browser's Curve25519 identity key.
pub struct VodozemacCrypto {
    /// The vodozemac Olm account.
    account: Account,
    /// Active Olm sessions keyed by peer identity key (Curve25519 base64).
    sessions: HashMap<String, Session>,
    /// Our Curve25519 identity key (base64 unpadded, cached).
    identity_key: String,
    /// Hub identifier.
    hub_id: String,
}

impl std::fmt::Debug for VodozemacCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VodozemacCrypto")
            .field("hub_id", &self.hub_id)
            .field("session_count", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

impl VodozemacCrypto {
    /// Create a new crypto manager with a fresh identity.
    pub fn new(hub_id: &str) -> Self {
        let account = Account::new();
        let identity_key = account.curve25519_key().to_base64();

        log::info!(
            "Created new VodozemacCrypto for hub {} (identity: {}...)",
            &hub_id[..hub_id.len().min(8)],
            &identity_key[..identity_key.len().min(16)]
        );

        Self {
            account,
            sessions: HashMap::new(),
            identity_key,
            hub_id: hub_id.to_string(),
        }
    }

    /// Load from persisted state, or create new if not found.
    pub fn load_or_create(hub_id: &str) -> Self {
        match Self::load(hub_id) {
            Ok(crypto) => {
                log::info!(
                    "Loaded existing VodozemacCrypto for hub {}",
                    &hub_id[..hub_id.len().min(8)]
                );
                crypto
            }
            Err(e) => {
                log::debug!("Could not load existing state: {e}, creating new");
                Self::new(hub_id)
            }
        }
    }

    /// Load from persisted state.
    fn load(hub_id: &str) -> Result<Self> {
        let state = persistence::load_vodozemac_crypto_store(hub_id)?;

        let pickle: vodozemac::olm::AccountPickle =
            serde_json::from_str(&state.pickled_account)
                .context("Failed to deserialize AccountPickle")?;
        let account = Account::from(pickle);
        let identity_key = account.curve25519_key().to_base64();

        let mut sessions = HashMap::new();

        for (peer_key, pickled) in &state.pickled_sessions {
            let pickle: vodozemac::olm::SessionPickle =
                serde_json::from_str(pickled)
                    .context("Failed to deserialize SessionPickle")?;
            sessions.insert(peer_key.clone(), Session::from(pickle));
        }

        Ok(Self {
            account,
            sessions,
            identity_key,
            hub_id: hub_id.to_string(),
        })
    }

    /// Build a `DeviceKeyBundle` for QR code display.
    ///
    /// Generates a fresh one-time key, signs the bundle with `Account.sign()`,
    /// and marks the key as published.
    pub fn build_device_key_bundle(&mut self) -> Result<DeviceKeyBundle> {
        let curve25519_key = self.account.curve25519_key().to_base64();
        let ed25519_key = self.account.ed25519_key().to_base64();

        // Generate a fresh one-time key
        self.account.generate_one_time_keys(1);
        let otk_map = self.account.one_time_keys();
        let (_key_id, otk) = otk_map
            .iter()
            .next()
            .context("No one-time key generated")?;
        let one_time_key = otk.to_base64();
        self.account.mark_keys_as_published();

        // Build the binary prefix that gets signed: [version][identity][signing][otk]
        let identity_bytes = decode_b64(&curve25519_key)?;
        let signing_bytes = decode_b64(&ed25519_key)?;
        let otk_bytes = decode_b64(&one_time_key)?;

        let mut sign_data = Vec::with_capacity(97);
        sign_data.push(PROTOCOL_VERSION);
        sign_data.extend_from_slice(&identity_bytes);
        sign_data.extend_from_slice(&signing_bytes);
        sign_data.extend_from_slice(&otk_bytes);

        let signature = self.account.sign(&sign_data);
        let signature_b64 = signature.to_base64();

        log::info!(
            "Built device key bundle, identity key {}...",
            &curve25519_key[..curve25519_key.len().min(16)]
        );

        Ok(DeviceKeyBundle {
            version: PROTOCOL_VERSION,
            hub_id: self.hub_id.clone(),
            curve25519_key,
            ed25519_key,
            one_time_key,
            signature: signature_b64,
        })
    }

    /// Get our Curve25519 identity key (base64 unpadded).
    pub fn identity_key(&self) -> &str {
        &self.identity_key
    }

    /// Check if we have an active Olm session for any peer.
    pub fn has_session(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Encrypt plaintext bytes for a specific peer, returning an `OlmEnvelope`.
    ///
    /// `peer_key` is the peer's Curve25519 identity key (base64).
    /// Requires an established session with that peer.
    pub fn encrypt(&mut self, plaintext: &[u8], peer_key: &str) -> Result<OlmEnvelope> {
        let session = self
            .sessions
            .get_mut(peer_key)
            .with_context(|| format!("No Olm session for peer {}...", &peer_key[..peer_key.len().min(16)]))?;

        let olm_message = session.encrypt(plaintext);

        let (message_type, ciphertext) = match olm_message {
            OlmMessage::PreKey(m) => (MSG_TYPE_PREKEY, STANDARD_NO_PAD.encode(m.to_bytes())),
            OlmMessage::Normal(m) => (MSG_TYPE_NORMAL, STANDARD_NO_PAD.encode(m.to_bytes())),
        };

        Ok(OlmEnvelope {
            message_type,
            ciphertext,
            sender_key: if message_type == MSG_TYPE_PREKEY {
                Some(self.identity_key.clone())
            } else {
                None
            },
        })
    }

    /// Decrypt an `OlmEnvelope`, returning plaintext bytes.
    ///
    /// For PreKey messages: looks up existing session by sender_key, or creates
    /// a new inbound session. Supports multiple concurrent browser sessions.
    /// For Normal messages: uses `peer_key` for direct lookup when available,
    /// otherwise falls back to trying all sessions.
    pub fn decrypt(&mut self, envelope: &OlmEnvelope, peer_key: Option<&str>) -> Result<Vec<u8>> {
        let ciphertext_bytes = STANDARD_NO_PAD
            .decode(&envelope.ciphertext)
            .context("Invalid base64 ciphertext")?;

        match envelope.message_type {
            MSG_TYPE_PREKEY => {
                let prekey_message = vodozemac::olm::PreKeyMessage::try_from(ciphertext_bytes.as_slice())
                    .map_err(|e| anyhow::anyhow!("Invalid PreKey message: {e}"))?;

                let sender_key = envelope
                    .sender_key
                    .as_deref()
                    .context("PreKey message missing sender_key")?;

                // Try existing session for this sender first (outbound session
                // sends ALL messages as PreKey until it receives a response).
                if let Some(session) = self.sessions.get_mut(sender_key) {
                    let olm_msg = OlmMessage::PreKey(prekey_message.clone());
                    match session.decrypt(&olm_msg) {
                        Ok(plaintext) => return Ok(plaintext),
                        Err(e) => {
                            log::debug!(
                                "Existing session for peer {}... couldn't decrypt PreKey (re-pairing?): {e}",
                                &sender_key[..sender_key.len().min(16)]
                            );
                        }
                    }
                }

                // No session for this sender, or existing one failed — create new.
                let sender_curve25519 = Curve25519PublicKey::from_base64(sender_key)
                    .map_err(|e| anyhow::anyhow!("Invalid sender Curve25519 key: {e}"))?;

                let result = self
                    .account
                    .create_inbound_session(sender_curve25519, &prekey_message)
                    .map_err(|e| anyhow::anyhow!("Failed to create inbound session: {e}"))?;

                self.sessions.insert(sender_key.to_string(), result.session);

                log::info!(
                    "Created inbound Olm session from PreKey (peer: {}..., total sessions: {})",
                    &sender_key[..sender_key.len().min(16)],
                    self.sessions.len()
                );

                Ok(result.plaintext)
            }
            MSG_TYPE_NORMAL => {
                let normal_message = vodozemac::olm::Message::try_from(ciphertext_bytes.as_slice())
                    .map_err(|e| anyhow::anyhow!("Invalid Normal message: {e}"))?;

                // Fast path: direct session lookup when peer key is known.
                if let Some(key) = peer_key {
                    if let Some(session) = self.sessions.get_mut(key) {
                        return session
                            .decrypt(&OlmMessage::Normal(normal_message))
                            .map_err(|e| anyhow::anyhow!("Decrypt failed for known peer: {e}"));
                    }
                    log::warn!(
                        "[CRYPTO] No session for peer_key {}..., falling back to scan ({} sessions)",
                        &key[..key.len().min(16)],
                        self.sessions.len()
                    );
                }

                // Slow path: try all sessions (no peer key hint available).
                for session in self.sessions.values_mut() {
                    match session.decrypt(&OlmMessage::Normal(normal_message.clone())) {
                        Ok(plaintext) => return Ok(plaintext),
                        Err(_) => continue,
                    }
                }

                anyhow::bail!(
                    "No session could decrypt Normal message ({} sessions tried)",
                    self.sessions.len()
                )
            }
            other => anyhow::bail!("Unknown message type: {other}"),
        }
    }

    // ========== Binary DataChannel API (zero base64, zero JSON) ==========

    /// Encrypt plaintext into a binary DataChannel frame for a specific peer.
    ///
    /// Output: `[message_type: 1][raw Olm ciphertext]` (Normal)
    /// or: `[message_type: 1][32-byte sender key][raw Olm ciphertext]` (PreKey).
    ///
    /// `peer_key` is the peer's Curve25519 identity key (base64).
    pub fn encrypt_binary(&mut self, plaintext: &[u8], peer_key: &str) -> Result<Vec<u8>> {
        let session = self
            .sessions
            .get_mut(peer_key)
            .with_context(|| format!("No Olm session for peer {}...", &peer_key[..peer_key.len().min(16)]))?;

        let olm_message = session.encrypt(plaintext);

        match olm_message {
            OlmMessage::PreKey(m) => {
                let ciphertext = m.to_bytes();
                let key_bytes = self.account.curve25519_key().to_bytes();
                let mut out = Vec::with_capacity(1 + 32 + ciphertext.len());
                out.push(MSG_TYPE_PREKEY);
                out.extend_from_slice(&key_bytes);
                out.extend_from_slice(&ciphertext);
                Ok(out)
            }
            OlmMessage::Normal(m) => {
                let ciphertext = m.to_bytes();
                let mut out = Vec::with_capacity(1 + ciphertext.len());
                out.push(MSG_TYPE_NORMAL);
                out.extend_from_slice(&ciphertext);
                Ok(out)
            }
        }
    }

    /// Decrypt a binary DataChannel frame, returning plaintext bytes.
    ///
    /// Input: `[message_type: 1][raw ciphertext]` (Normal)
    /// or: `[message_type: 1][32-byte sender key][raw ciphertext]` (PreKey).
    ///
    /// When `peer_key` is provided, Normal messages use direct session lookup
    /// instead of iterating all sessions (O(1) vs O(n) where each miss costs
    /// an HMAC check).
    pub fn decrypt_binary(&mut self, data: &[u8], peer_key: Option<&str>) -> Result<Vec<u8>> {
        anyhow::ensure!(!data.is_empty(), "Empty binary frame");

        let msg_type = data[0];
        match msg_type {
            MSG_TYPE_PREKEY => {
                anyhow::ensure!(data.len() > 33, "PreKey frame too short");
                let sender_key_bytes: [u8; 32] = data[1..33]
                    .try_into()
                    .expect("slice is exactly 32 bytes");
                let sender_curve25519 = Curve25519PublicKey::from_bytes(sender_key_bytes);
                let sender_key_b64 = sender_curve25519.to_base64();
                let ciphertext = &data[33..];

                let prekey_message =
                    vodozemac::olm::PreKeyMessage::try_from(ciphertext)
                        .map_err(|e| anyhow::anyhow!("Invalid PreKey message: {e}"))?;

                // Try existing session for this sender first.
                if let Some(session) = self.sessions.get_mut(&sender_key_b64) {
                    let olm_msg = OlmMessage::PreKey(prekey_message.clone());
                    match session.decrypt(&olm_msg) {
                        Ok(plaintext) => return Ok(plaintext),
                        Err(e) => {
                            log::debug!(
                                "Existing session for peer {}... couldn't decrypt PreKey: {e}",
                                &sender_key_b64[..sender_key_b64.len().min(16)]
                            );
                        }
                    }
                }

                let result = self
                    .account
                    .create_inbound_session(sender_curve25519, &prekey_message)
                    .map_err(|e| anyhow::anyhow!("Failed to create inbound session: {e}"))?;

                self.sessions.insert(sender_key_b64.clone(), result.session);

                log::info!(
                    "Created inbound Olm session from binary PreKey (peer: {}..., total sessions: {})",
                    &sender_key_b64[..sender_key_b64.len().min(16)],
                    self.sessions.len()
                );

                Ok(result.plaintext)
            }
            MSG_TYPE_NORMAL => {
                let ciphertext = &data[1..];
                let normal_message =
                    vodozemac::olm::Message::try_from(ciphertext)
                        .map_err(|e| anyhow::anyhow!("Invalid Normal message: {e}"))?;

                // Fast path: direct session lookup when peer key is known.
                if let Some(key) = peer_key {
                    if let Some(session) = self.sessions.get_mut(key) {
                        return session
                            .decrypt(&OlmMessage::Normal(normal_message))
                            .map_err(|e| anyhow::anyhow!("Decrypt failed for known peer: {e}"));
                    }
                    log::warn!(
                        "[CRYPTO] No session for peer_key {}..., falling back to scan ({} sessions)",
                        &key[..key.len().min(16)],
                        self.sessions.len()
                    );
                }

                // Slow path: try all sessions (no peer key hint available).
                for session in self.sessions.values_mut() {
                    match session.decrypt(&OlmMessage::Normal(normal_message.clone())) {
                        Ok(plaintext) => return Ok(plaintext),
                        Err(_) => continue,
                    }
                }

                anyhow::bail!(
                    "No session could decrypt binary Normal message ({} sessions tried)",
                    self.sessions.len()
                )
            }
            other => anyhow::bail!("Unknown binary message type: 0x{other:02x}"),
        }
    }

    // ========== End Binary DataChannel API ==========

    /// Persist crypto state to disk.
    pub fn persist(&self) -> Result<()> {
        let pickled_account = serde_json::to_string(&self.account.pickle())
            .context("Failed to serialize AccountPickle")?;

        let mut pickled_sessions = HashMap::new();
        for (peer_key, session) in &self.sessions {
            let pickled = serde_json::to_string(&session.pickle())
                .context("Failed to serialize SessionPickle")?;
            pickled_sessions.insert(peer_key.clone(), pickled);
        }

        let state = VodozemacCryptoState {
            pickled_account,
            hub_id: self.hub_id.clone(),
            pickled_sessions,
        };

        persistence::save_vodozemac_crypto_store(&self.hub_id, &state)?;

        log::debug!(
            "Persisted vodozemac crypto state for hub {} ({} sessions)",
            &self.hub_id[..self.hub_id.len().min(8)],
            self.sessions.len()
        );

        Ok(())
    }

    /// Remove the Olm session for a specific peer.
    ///
    /// Called during ratchet restart to clear stale session state
    /// before the peer creates a new outbound session from a fresh bundle.
    ///
    /// Returns `true` if a session was removed.
    pub fn remove_session(&mut self, peer_key: &str) -> bool {
        let removed = self.sessions.remove(peer_key).is_some();
        if removed {
            log::info!(
                "Removed Olm session for peer {}... ({} sessions remaining)",
                &peer_key[..peer_key.len().min(16)],
                self.sessions.len()
            );
        }
        removed
    }

    /// Generate a fresh bundle and clear the old session for a peer.
    ///
    /// This is the core of ratchet restart: generates a new one-time key,
    /// builds a `DeviceKeyBundle`, and removes the old session so the
    /// peer can establish a fresh one.
    ///
    /// Returns the bundle as raw binary bytes (161 bytes) for sending
    /// as a type-2 (bundle refresh) message.
    pub fn refresh_bundle_for_peer(&mut self, peer_key: &str) -> Result<Vec<u8>> {
        self.remove_session(peer_key);

        let bundle = self.build_device_key_bundle()?;
        let bundle_bytes = bundle.to_binary()?;

        log::info!(
            "Generated refresh bundle for peer {}... (identity: {}...)",
            &peer_key[..peer_key.len().min(16)],
            &self.identity_key[..self.identity_key.len().min(16)]
        );

        Ok(bundle_bytes)
    }
}

#[cfg(test)]
impl VodozemacCrypto {
    /// Create an outbound session to a peer (test helper).
    ///
    /// In production, the browser creates the outbound session; the CLI
    /// always uses inbound sessions created from PreKey messages.
    pub fn create_outbound_session(
        &mut self,
        peer_identity_key: &str,
        peer_one_time_key: &str,
    ) -> Result<()> {
        let identity = Curve25519PublicKey::from_base64(peer_identity_key)
            .map_err(|e| anyhow::anyhow!("Invalid peer identity key: {e}"))?;
        let otk = Curve25519PublicKey::from_base64(peer_one_time_key)
            .map_err(|e| anyhow::anyhow!("Invalid peer one-time key: {e}"))?;

        let session = self
            .account
            .create_outbound_session(SessionConfig::version_2(), identity, otk);

        self.sessions.insert(peer_identity_key.to_string(), session);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_creation() {
        let crypto = VodozemacCrypto::new("test-hub");
        assert!(!crypto.identity_key().is_empty());
        assert!(!crypto.has_session());
    }

    #[test]
    fn test_device_key_bundle_generation() {
        let mut crypto = VodozemacCrypto::new("test-hub-bundle");
        let bundle = crypto.build_device_key_bundle().unwrap();

        assert_eq!(bundle.version, PROTOCOL_VERSION);
        assert!(!bundle.curve25519_key.is_empty());
        assert!(!bundle.ed25519_key.is_empty());
        assert!(!bundle.one_time_key.is_empty());
        assert!(!bundle.signature.is_empty());
    }

    #[test]
    fn test_bundle_binary_round_trip() {
        let mut crypto = VodozemacCrypto::new("test-hub-binary");
        let bundle = crypto.build_device_key_bundle().unwrap();

        let bytes = bundle.to_binary().unwrap();
        assert_eq!(bytes.len(), binary_format::BUNDLE_SIZE);

        let restored = DeviceKeyBundle::from_binary(&bytes).unwrap();

        assert_eq!(bundle.version, restored.version);
        assert_eq!(bundle.curve25519_key, restored.curve25519_key);
        assert_eq!(bundle.ed25519_key, restored.ed25519_key);
        assert_eq!(bundle.one_time_key, restored.one_time_key);
        assert_eq!(bundle.signature, restored.signature);
    }

    /// Verify the browser's signature verification approach works:
    /// extract raw bytes from the binary bundle and verify with Ed25519.
    #[test]
    fn test_bundle_signature_verification_raw_bytes() {
        use vodozemac::{Ed25519PublicKey, Ed25519Signature};

        let mut crypto = VodozemacCrypto::new("test-hub-sig-verify");
        let bundle = crypto.build_device_key_bundle().unwrap();
        let bytes = bundle.to_binary().unwrap();

        // Mimic browser's parseBinaryBundle():
        // signedData = bytes[0..97], signingKeyRaw = bytes[33..65], signatureRaw = bytes[97..161]
        let signed_data = &bytes[0..97];
        let signing_key_raw: &[u8; 32] = bytes[33..65].try_into().unwrap();
        let signature_raw = &bytes[97..161];

        let key = Ed25519PublicKey::from_slice(signing_key_raw).unwrap();
        let sig = Ed25519Signature::from_slice(signature_raw).unwrap();

        key.verify(signed_data, &sig)
            .expect("Bundle signature should verify with raw byte extraction");
    }

    #[test]
    fn test_bundle_fixed_size() {
        let mut crypto = VodozemacCrypto::new("test-hub-size");
        let bundle = crypto.build_device_key_bundle().unwrap();
        let bytes = bundle.to_binary().unwrap();

        assert_eq!(
            bytes.len(),
            161,
            "v6 bundle should be exactly 161 bytes"
        );
    }

    #[test]
    fn test_bundle_fits_qr() {
        use data_encoding::BASE32_NOPAD;

        let mut crypto = VodozemacCrypto::new("test-hub-qr");
        let bundle = crypto.build_device_key_bundle().unwrap();

        let bytes = bundle.to_binary().unwrap();
        let base32 = BASE32_NOPAD.encode(&bytes);
        let url = format!("HTTPS://BOTSTER.DEV/H/123#{}", base32);

        assert!(
            url.len() < 1000,
            "URL should be under 1000 chars, got {}",
            url.len()
        );
    }

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        // Create two accounts to simulate CLI and browser
        let mut cli = VodozemacCrypto::new("test-roundtrip-cli");
        let mut browser = VodozemacCrypto::new("test-roundtrip-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        // CLI generates bundle, browser creates outbound session
        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();

        // Browser encrypts (PreKey message)
        let plaintext = b"Hello from browser!";
        let envelope = browser.encrypt(plaintext, &cli_key).unwrap();
        assert_eq!(envelope.message_type, MSG_TYPE_PREKEY);

        // CLI decrypts (creates inbound session)
        let decrypted = cli.decrypt(&envelope, None).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(cli.has_session());

        // CLI encrypts back (Normal message)
        let reply = b"Hello from CLI!";
        let reply_envelope = cli.encrypt(reply, &browser_key).unwrap();
        assert_eq!(reply_envelope.message_type, MSG_TYPE_NORMAL);

        // Browser decrypts
        let reply_decrypted = browser.decrypt(&reply_envelope, None).unwrap();
        assert_eq!(reply_decrypted, reply);
    }

    #[test]
    fn test_envelope_serialization() {
        let envelope = OlmEnvelope {
            message_type: MSG_TYPE_NORMAL,
            ciphertext: "dGVzdA".to_string(),
            sender_key: None,
        };

        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains(r#""t":1"#));
        assert!(!json.contains(r#""k""#), "sender_key should be skipped when None");

        let restored: OlmEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope.message_type, restored.message_type);
        assert_eq!(envelope.ciphertext, restored.ciphertext);
        assert!(restored.sender_key.is_none());
    }

    #[test]
    fn test_prekey_envelope_includes_sender_key() {
        let envelope = OlmEnvelope {
            message_type: MSG_TYPE_PREKEY,
            ciphertext: "dGVzdA".to_string(),
            sender_key: Some("sender_key_here".to_string()),
        };

        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains(r#""k":"sender_key_here""#));
    }

    #[test]
    fn test_binary_format_deterministic() {
        let bundle = DeviceKeyBundle {
            version: PROTOCOL_VERSION,
            hub_id: "ignored".to_string(),
            curve25519_key: STANDARD_NO_PAD.encode([1u8; 32]),
            ed25519_key: STANDARD_NO_PAD.encode([2u8; 32]),
            one_time_key: STANDARD_NO_PAD.encode([3u8; 32]),
            signature: STANDARD_NO_PAD.encode([4u8; 64]),
        };

        let bytes1 = bundle.to_binary().unwrap();
        let bytes2 = bundle.to_binary().unwrap();

        assert_eq!(bytes1, bytes2, "Binary serialization should be deterministic");
    }

    /// Verify CLI can decrypt multiple PreKey messages from the same outbound
    /// session. This happens when the browser sends offer + ICE candidates +
    /// DataChannel subscribe before receiving the CLI's first reply (the
    /// outbound session keeps producing PreKey messages until ratcheted).
    #[test]
    fn test_multiple_prekey_messages_before_reply() {
        let mut cli = VodozemacCrypto::new("test-multi-prekey-cli");
        let mut browser = VodozemacCrypto::new("test-multi-prekey-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();

        // Browser sends 5 messages (all PreKey) before CLI replies.
        // This simulates: SDP offer, ICE candidate x3, subscribe.
        let mut envelopes = Vec::new();
        for i in 0..5 {
            let msg = format!("browser msg {i}");
            let env = browser.encrypt(msg.as_bytes(), &cli_key).unwrap();
            assert_eq!(env.message_type, MSG_TYPE_PREKEY, "msg {i} should be PreKey");
            envelopes.push((msg, env));
        }

        // CLI decrypts all 5 — first creates inbound session, rest use it.
        for (i, (msg, env)) in envelopes.iter().enumerate() {
            let decrypted = cli.decrypt(env, None).unwrap();
            assert_eq!(decrypted, msg.as_bytes(), "msg {i} decryption mismatch");
        }

        // CLI replies (Normal). Browser decrypts → session ratchets.
        let reply_env = cli.encrypt(b"cli reply", &browser_key).unwrap();
        assert_eq!(reply_env.message_type, MSG_TYPE_NORMAL);
        let reply_dec = browser.decrypt(&reply_env, None).unwrap();
        assert_eq!(reply_dec, b"cli reply");

        // Browser's subsequent messages are now Normal.
        let post_ratchet = browser.encrypt(b"normal now", &cli_key).unwrap();
        assert_eq!(post_ratchet.message_type, MSG_TYPE_NORMAL);
        let dec = cli.decrypt(&post_ratchet, None).unwrap();
        assert_eq!(dec, b"normal now");
    }

    #[test]
    fn test_multiple_messages_after_session() {
        let mut cli = VodozemacCrypto::new("test-multi-cli");
        let mut browser = VodozemacCrypto::new("test-multi-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();

        // Establish session
        let envelope = browser.encrypt(b"first", &cli_key).unwrap();
        let _ = cli.decrypt(&envelope, None).unwrap();

        // Multiple messages in both directions
        for i in 0..5 {
            let msg = format!("cli message {i}");
            let env = cli.encrypt(msg.as_bytes(), &browser_key).unwrap();
            assert_eq!(env.message_type, MSG_TYPE_NORMAL);
            let dec = browser.decrypt(&env, None).unwrap();
            assert_eq!(dec, msg.as_bytes());

            let msg2 = format!("browser message {i}");
            let env2 = browser.encrypt(msg2.as_bytes(), &cli_key).unwrap();
            assert_eq!(env2.message_type, MSG_TYPE_NORMAL);
            let dec2 = cli.decrypt(&env2, None).unwrap();
            assert_eq!(dec2, msg2.as_bytes());
        }
    }

    #[test]
    fn test_binary_encrypt_decrypt_round_trip() {
        let mut cli = VodozemacCrypto::new("test-binary-rt-cli");
        let mut browser = VodozemacCrypto::new("test-binary-rt-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();

        // Browser encrypts binary (PreKey)
        let plaintext = b"binary payload";
        let frame = browser.encrypt_binary(plaintext, &cli_key).unwrap();
        assert_eq!(frame[0], MSG_TYPE_PREKEY);
        // PreKey frame: [0x00][32 sender key][ciphertext]
        assert!(frame.len() > 33);

        // CLI decrypts binary
        let decrypted = cli.decrypt_binary(&frame, None).unwrap();
        assert_eq!(decrypted, plaintext);

        // CLI encrypts binary back (Normal)
        let reply = b"binary reply";
        let reply_frame = cli.encrypt_binary(reply, &browser_key).unwrap();
        assert_eq!(reply_frame[0], MSG_TYPE_NORMAL);
        // Normal frame: [0x01][ciphertext]

        // Browser decrypts binary
        let reply_dec = browser.decrypt_binary(&reply_frame, None).unwrap();
        assert_eq!(reply_dec, reply);

        // Multiple binary round-trips
        for i in 0..5 {
            let msg = format!("binary msg {i}");
            let f = cli.encrypt_binary(msg.as_bytes(), &browser_key).unwrap();
            assert_eq!(f[0], MSG_TYPE_NORMAL);
            let d = browser.decrypt_binary(&f, None).unwrap();
            assert_eq!(d, msg.as_bytes());
        }
    }

    /// Two browser devices connecting to the same CLI simultaneously.
    /// Each device gets its own Olm session; messages don't interfere.
    #[test]
    fn test_multi_device_concurrent_sessions() {
        let mut cli = VodozemacCrypto::new("test-multi-device-cli");
        let mut desktop = VodozemacCrypto::new("test-multi-device-desktop");
        let mut phone = VodozemacCrypto::new("test-multi-device-phone");
        let cli_key = cli.identity_key().to_string();
        let desktop_key = desktop.identity_key().to_string();
        let phone_key = phone.identity_key().to_string();

        // Desktop pairs first.
        let bundle1 = cli.build_device_key_bundle().unwrap();
        desktop
            .create_outbound_session(&bundle1.curve25519_key, &bundle1.one_time_key)
            .unwrap();
        let env1 = desktop.encrypt(b"hello from desktop", &cli_key).unwrap();
        let dec1 = cli.decrypt(&env1, None).unwrap();
        assert_eq!(dec1, b"hello from desktop");

        // Phone pairs second (needs a fresh OTK).
        let bundle2 = cli.build_device_key_bundle().unwrap();
        phone
            .create_outbound_session(&bundle2.curve25519_key, &bundle2.one_time_key)
            .unwrap();
        let env2 = phone.encrypt(b"hello from phone", &cli_key).unwrap();
        let dec2 = cli.decrypt(&env2, None).unwrap();
        assert_eq!(dec2, b"hello from phone");

        // CLI should have 2 sessions.
        assert_eq!(cli.sessions.len(), 2);

        // CLI can reply to each device independently.
        let reply_desktop = cli.encrypt(b"reply to desktop", &desktop_key).unwrap();
        let reply_phone = cli.encrypt(b"reply to phone", &phone_key).unwrap();

        assert_eq!(desktop.decrypt(&reply_desktop, None).unwrap(), b"reply to desktop");
        assert_eq!(phone.decrypt(&reply_phone, None).unwrap(), b"reply to phone");

        // Continued messages from desktop still work (session not broken by phone).
        let env3 = desktop.encrypt(b"desktop still here", &cli_key).unwrap();
        assert_eq!(cli.decrypt(&env3, None).unwrap(), b"desktop still here");
    }

    #[test]
    fn test_binary_content_format() {
        // Verify binary inner content: [type][flags][sub_len][sub_id][payload]
        let sub_id = "hub:1:Terminal:0:0";
        let payload = b"raw pty output";
        let sub_bytes = sub_id.as_bytes();

        let mut content = Vec::new();
        content.push(CONTENT_PTY); // 0x01
        content.push(0x01); // flags: compressed
        content.push(sub_bytes.len() as u8);
        content.extend_from_slice(sub_bytes);
        content.extend_from_slice(payload);

        // Parse it back
        assert_eq!(content[0], CONTENT_PTY);
        assert_eq!(content[1] & 0x01, 0x01); // compressed flag
        let len = content[2] as usize;
        let parsed_sub = std::str::from_utf8(&content[3..3 + len]).unwrap();
        assert_eq!(parsed_sub, sub_id);
        assert_eq!(&content[3 + len..], payload);
    }

    #[test]
    fn test_remove_session() {
        let mut cli = VodozemacCrypto::new("test-remove-cli");
        let mut browser = VodozemacCrypto::new("test-remove-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        // Establish session
        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();
        let env = browser.encrypt(b"hello", &cli_key).unwrap();
        cli.decrypt(&env, None).unwrap();
        assert!(cli.has_session());

        // Remove session
        assert!(cli.remove_session(&browser_key));
        assert!(!cli.has_session());

        // Removing again returns false
        assert!(!cli.remove_session(&browser_key));
    }

    #[test]
    fn test_refresh_bundle_for_peer() {
        let mut cli = VodozemacCrypto::new("test-refresh-cli");
        let mut browser = VodozemacCrypto::new("test-refresh-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        // Establish initial session
        let bundle1 = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle1.curve25519_key, &bundle1.one_time_key)
            .unwrap();
        let env = browser.encrypt(b"initial", &cli_key).unwrap();
        cli.decrypt(&env, None).unwrap();

        // Refresh bundle — old session cleared, new OTK generated
        let refresh_bytes = cli.refresh_bundle_for_peer(&browser_key).unwrap();
        assert_eq!(refresh_bytes.len(), binary_format::BUNDLE_SIZE);
        assert!(!cli.has_session());

        // Parse refresh bundle — identity key same, OTK different
        let refresh_bundle = DeviceKeyBundle::from_binary(&refresh_bytes).unwrap();
        assert_eq!(refresh_bundle.curve25519_key, bundle1.curve25519_key);
        assert_ne!(refresh_bundle.one_time_key, bundle1.one_time_key);
    }

    /// Full ratchet restart round-trip: establish → desync → refresh → re-establish.
    #[test]
    fn test_ratchet_restart_full_flow() {
        let mut cli = VodozemacCrypto::new("test-restart-cli");
        let mut browser = VodozemacCrypto::new("test-restart-browser");
        let cli_key = cli.identity_key().to_string();
        let browser_key = browser.identity_key().to_string();

        // Step 1: Normal session establishment
        let bundle = cli.build_device_key_bundle().unwrap();
        browser
            .create_outbound_session(&bundle.curve25519_key, &bundle.one_time_key)
            .unwrap();
        let env = browser.encrypt(b"hello", &cli_key).unwrap();
        cli.decrypt(&env, None).unwrap();
        let reply = cli.encrypt(b"world", &browser_key).unwrap();
        browser.decrypt(&reply, None).unwrap();

        // Step 2: CLI refreshes bundle (simulates desync detection)
        let refresh_bytes = cli.refresh_bundle_for_peer(&browser_key).unwrap();
        let refresh_bundle = DeviceKeyBundle::from_binary(&refresh_bytes).unwrap();

        // Step 3: Browser creates new outbound session from fresh bundle
        // (In production, handleCreateSession clears old state and recreates)
        let mut browser2 = VodozemacCrypto::new("test-restart-browser2");
        browser2
            .create_outbound_session(
                &refresh_bundle.curve25519_key,
                &refresh_bundle.one_time_key,
            )
            .unwrap();

        // Step 4: Browser sends PreKey with new session
        let prekey_env = browser2.encrypt(b"back online", &cli_key).unwrap();
        assert_eq!(prekey_env.message_type, MSG_TYPE_PREKEY);

        // CLI creates inbound session from PreKey
        let dec = cli.decrypt(&prekey_env, None).unwrap();
        assert_eq!(dec, b"back online");

        // Step 5: Bidirectional communication restored
        let browser2_key = browser2.identity_key().to_string();
        let cli_reply = cli.encrypt(b"welcome back", &browser2_key).unwrap();
        let dec2 = browser2.decrypt(&cli_reply, None).unwrap();
        assert_eq!(dec2, b"welcome back");
    }
}
