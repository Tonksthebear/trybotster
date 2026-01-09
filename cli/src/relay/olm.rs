//! Olm E2E Encryption - vodozemac wrapper.
//!
//! This module provides E2E encryption using vodozemac's Olm implementation,
//! which is the same battle-tested, NCC-audited cryptography used by Matrix.
//!
//! # Protocol Flow
//!
//! ```text
//! CLI (Server)                              Browser (Client)
//! ──────────────────────────────────────────────────────────
//! 1. Generate Account
//! 2. Generate one-time key
//! 3. Display QR code with:
//!    - ed25519 (signing key)
//!    - curve25519 (identity key)
//!    - one_time_key
//!
//!                                   4. Scan QR, get keys
//!                                   5. Create outbound session
//!                                   6. Send PreKey message ──►
//!
//! 7. Receive PreKey message
//! 8. Create inbound session
//! 9. Both sides now have session
//!
//!    ◄── Normal messages ──►
//! ```
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use vodozemac::olm::{
    Account as VodozemacAccount, InboundCreationResult, OlmMessage, Session as VodozemacSession,
};
#[cfg(test)]
use vodozemac::olm::SessionConfig;
use vodozemac::{Curve25519PublicKey, Ed25519PublicKey};

/// Keys needed for session establishment, included in QR code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEstablishmentKeys {
    /// Ed25519 signing key (base64)
    pub ed25519: String,
    /// Curve25519 identity key (base64)
    pub curve25519: String,
    /// One-time key for this session (base64)
    pub one_time_key: String,
}

/// Encrypted Olm message envelope (protocol v3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OlmEnvelope {
    /// Protocol version (3 for Olm)
    pub version: u8,
    /// Message type: 0 = PreKey, 1 = Normal
    pub message_type: u8,
    /// Base64-encoded ciphertext
    pub ciphertext: String,
    /// Sender's Curve25519 identity key (base64)
    pub sender_key: String,
}

impl OlmEnvelope {
    /// Current protocol version for Olm messages.
    pub const VERSION: u8 = 3;
}

/// Olm Account wrapper - holds identity keys and one-time keys.
///
/// The Account is the long-lived identity. It generates one-time keys
/// that are used to establish sessions.
pub struct OlmAccount {
    inner: VodozemacAccount,
}

impl std::fmt::Debug for OlmAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys = self.inner.identity_keys();
        f.debug_struct("OlmAccount")
            .field("ed25519", &keys.ed25519.to_base64())
            .field("curve25519", &keys.curve25519.to_base64())
            .finish_non_exhaustive()
    }
}

impl OlmAccount {
    /// Create a new Olm account with fresh identity keys.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: VodozemacAccount::new(),
        }
    }

    /// Restore an account from a pickle string.
    pub fn from_pickle(pickle: &str) -> Result<Self> {
        let account_pickle = serde_json::from_str(pickle)
            .context("Failed to parse account pickle")?;
        let account = VodozemacAccount::from_pickle(account_pickle);
        Ok(Self { inner: account })
    }

    /// Pickle (serialize) the account for storage.
    #[must_use]
    pub fn pickle(&self) -> String {
        let pickle = self.inner.pickle();
        serde_json::to_string(&pickle).expect("Failed to serialize account pickle")
    }

    /// Get the Ed25519 signing key (base64).
    #[must_use]
    pub fn ed25519_key(&self) -> String {
        self.inner.identity_keys().ed25519.to_base64()
    }

    /// Get the Curve25519 identity key (base64).
    #[must_use]
    pub fn curve25519_key(&self) -> String {
        self.inner.identity_keys().curve25519.to_base64()
    }

    /// Generate one-time keys for session establishment.
    pub fn generate_one_time_keys(&mut self, count: usize) {
        self.inner.generate_one_time_keys(count);
    }

    /// Get the current one-time keys (base64).
    ///
    /// Returns a list of (key_id, key) tuples.
    #[must_use]
    pub fn one_time_keys(&self) -> Vec<(String, String)> {
        self.inner
            .one_time_keys()
            .into_iter()
            .map(|(id, key)| (id.to_base64(), key.to_base64()))
            .collect()
    }

    /// Get a single one-time key for session establishment.
    ///
    /// Returns None if no one-time keys are available.
    #[must_use]
    pub fn get_one_time_key(&self) -> Option<String> {
        self.inner
            .one_time_keys()
            .into_iter()
            .next()
            .map(|(_, key)| key.to_base64())
    }

    /// Mark one-time keys as published (consumed).
    pub fn mark_keys_as_published(&mut self) {
        self.inner.mark_keys_as_published();
    }

    /// Get the keys needed for QR code session establishment.
    ///
    /// Generates a new one-time key if needed.
    pub fn session_establishment_keys(&mut self) -> SessionEstablishmentKeys {
        // Ensure we have at least one one-time key
        if self.inner.one_time_keys().is_empty() {
            self.inner.generate_one_time_keys(1);
        }

        let one_time_key = self.get_one_time_key()
            .expect("Should have at least one key after generation");

        SessionEstablishmentKeys {
            ed25519: self.ed25519_key(),
            curve25519: self.curve25519_key(),
            one_time_key,
        }
    }

    /// Create an inbound session from a PreKey message.
    ///
    /// This is called when receiving the first message from a new peer.
    pub fn create_inbound_session(
        &mut self,
        sender_curve25519: &str,
        prekey_message: &OlmEnvelope,
    ) -> Result<(OlmSession, Vec<u8>)> {
        if prekey_message.message_type != 0 {
            anyhow::bail!("Expected PreKey message (type 0), got type {}", prekey_message.message_type);
        }

        let sender_key = Curve25519PublicKey::from_base64(sender_curve25519)
            .context("Invalid sender Curve25519 key")?;

        let ciphertext = BASE64.decode(&prekey_message.ciphertext)
            .context("Invalid base64 ciphertext")?;

        let olm_message = vodozemac::olm::PreKeyMessage::try_from(ciphertext.as_slice())
            .context("Invalid PreKey message format")?;

        let InboundCreationResult { session, plaintext } = self
            .inner
            .create_inbound_session(sender_key, &olm_message)
            .context("Failed to create inbound session")?;

        // Mark the one-time key as used
        self.mark_keys_as_published();

        Ok((OlmSession { inner: session, peer_curve25519: sender_curve25519.to_string() }, plaintext))
    }

    /// Sign a message with the account's Ed25519 key.
    #[must_use]
    pub fn sign(&self, message: &str) -> String {
        self.inner.sign(message).to_base64()
    }

    /// Create an outbound session (test-only, simulates browser side).
    ///
    /// This is used in tests to simulate the browser creating an outbound session
    /// to the CLI. In production, this is done by the browser WASM module.
    #[cfg(test)]
    pub fn create_outbound_session(
        &self,
        peer_identity_key: &str,
        peer_one_time_key: &str,
    ) -> Result<VodozemacSession> {
        let identity_key = Curve25519PublicKey::from_base64(peer_identity_key)
            .context("Invalid peer identity key")?;
        let one_time_key = Curve25519PublicKey::from_base64(peer_one_time_key)
            .context("Invalid peer one-time key")?;
        Ok(self.inner.create_outbound_session(
            SessionConfig::version_2(),
            identity_key,
            one_time_key,
        ))
    }
}

impl Default for OlmAccount {
    fn default() -> Self {
        Self::new()
    }
}

/// Olm Session - for encrypting/decrypting messages with a peer.
///
/// Created either:
/// - By the peer using `create_outbound_session` (they send PreKey)
/// - By us using `create_inbound_session` (we receive PreKey)
pub struct OlmSession {
    inner: VodozemacSession,
    /// Peer's Curve25519 identity key for envelope construction.
    peer_curve25519: String,
}

impl std::fmt::Debug for OlmSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OlmSession")
            .field("session_id", &self.inner.session_id())
            .field("peer_curve25519", &self.peer_curve25519)
            .finish_non_exhaustive()
    }
}

impl OlmSession {
    /// Restore a session from a pickle string.
    pub fn from_pickle(pickle: &str, peer_curve25519: String) -> Result<Self> {
        let session_pickle = serde_json::from_str(pickle)
            .context("Failed to parse session pickle")?;
        let session = VodozemacSession::from_pickle(session_pickle);
        Ok(Self { inner: session, peer_curve25519 })
    }

    /// Pickle (serialize) the session for storage.
    #[must_use]
    pub fn pickle(&self) -> String {
        let pickle = self.inner.pickle();
        serde_json::to_string(&pickle).expect("Failed to serialize session pickle")
    }

    /// Get the session ID.
    #[must_use]
    pub fn session_id(&self) -> String {
        self.inner.session_id()
    }

    /// Get the peer's Curve25519 identity key.
    #[must_use]
    pub fn peer_curve25519(&self) -> &str {
        &self.peer_curve25519
    }

    /// Encrypt a message.
    ///
    /// Returns an `OlmEnvelope` ready to send.
    #[must_use]
    pub fn encrypt(&mut self, plaintext: &[u8], our_curve25519: &str) -> OlmEnvelope {
        let message = self.inner.encrypt(plaintext);

        let (message_type, ciphertext) = match message {
            OlmMessage::PreKey(m) => (0, BASE64.encode(m.to_bytes())),
            OlmMessage::Normal(m) => (1, BASE64.encode(m.to_bytes())),
        };

        OlmEnvelope {
            version: OlmEnvelope::VERSION,
            message_type,
            ciphertext,
            sender_key: our_curve25519.to_string(),
        }
    }

    /// Decrypt a message.
    pub fn decrypt(&mut self, envelope: &OlmEnvelope) -> Result<Vec<u8>> {
        let ciphertext = BASE64.decode(&envelope.ciphertext)
            .context("Invalid base64 ciphertext")?;

        let olm_message = match envelope.message_type {
            0 => {
                let m = vodozemac::olm::PreKeyMessage::try_from(ciphertext.as_slice())
                    .context("Invalid PreKey message")?;
                OlmMessage::PreKey(m)
            }
            1 => {
                let m = vodozemac::olm::Message::try_from(ciphertext.as_slice())
                    .context("Invalid Normal message")?;
                OlmMessage::Normal(m)
            }
            other => anyhow::bail!("Unknown message type: {}", other),
        };

        self.inner
            .decrypt(&olm_message)
            .context("Decryption failed")
    }
}

/// Verify an Ed25519 signature.
pub fn verify_signature(public_key: &str, message: &str, signature: &str) -> Result<bool> {
    let key = Ed25519PublicKey::from_base64(public_key)
        .context("Invalid Ed25519 public key")?;
    let sig = vodozemac::Ed25519Signature::from_base64(signature)
        .context("Invalid signature format")?;

    Ok(key.verify(message.as_bytes(), &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_creation() {
        let account = OlmAccount::new();
        assert!(!account.ed25519_key().is_empty());
        assert!(!account.curve25519_key().is_empty());
    }

    #[test]
    fn test_account_pickle() {
        let account = OlmAccount::new();
        let ed25519 = account.ed25519_key();

        let pickle = account.pickle();
        let restored = OlmAccount::from_pickle(&pickle).expect("Failed to restore");

        assert_eq!(ed25519, restored.ed25519_key());
    }

    #[test]
    fn test_session_establishment_keys() {
        let mut account = OlmAccount::new();
        let keys = account.session_establishment_keys();

        assert!(!keys.ed25519.is_empty());
        assert!(!keys.curve25519.is_empty());
        assert!(!keys.one_time_key.is_empty());
    }

    #[test]
    fn test_sign_and_verify() {
        let account = OlmAccount::new();
        let message = "test message";
        let signature = account.sign(message);

        let valid = verify_signature(&account.ed25519_key(), message, &signature)
            .expect("Verification failed");
        assert!(valid);

        // Wrong message should fail
        let invalid = verify_signature(&account.ed25519_key(), "wrong message", &signature)
            .expect("Verification failed");
        assert!(!invalid);
    }

    #[test]
    fn test_full_session_flow() {
        // CLI creates account and generates one-time key
        let mut cli_account = OlmAccount::new();
        cli_account.generate_one_time_keys(1);
        let cli_identity = cli_account.curve25519_key();
        let cli_otk = cli_account.get_one_time_key().unwrap();

        // Browser creates account and outbound session
        let browser_account = OlmAccount::new();
        let browser_identity = browser_account.curve25519_key();

        // Simulate browser creating outbound session (normally done in WASM)
        // We'll use the raw vodozemac API here for testing
        let cli_identity_key = Curve25519PublicKey::from_base64(&cli_identity).unwrap();
        let cli_one_time_key = Curve25519PublicKey::from_base64(&cli_otk).unwrap();
        let mut browser_session = browser_account.inner.create_outbound_session(
            SessionConfig::version_2(),
            cli_identity_key,
            cli_one_time_key,
        );

        // Browser sends first message (PreKey)
        let plaintext = b"Hello CLI!";
        let message = browser_session.encrypt(plaintext);
        let (message_type, ciphertext) = match message {
            OlmMessage::PreKey(m) => (0u8, BASE64.encode(m.to_bytes())),
            OlmMessage::Normal(m) => (1u8, BASE64.encode(m.to_bytes())),
        };
        assert_eq!(message_type, 0); // First message should be PreKey

        let envelope = OlmEnvelope {
            version: OlmEnvelope::VERSION,
            message_type,
            ciphertext,
            sender_key: browser_identity.clone(),
        };

        // CLI receives and creates inbound session
        let (mut cli_session, decrypted) = cli_account
            .create_inbound_session(&browser_identity, &envelope)
            .expect("Failed to create inbound session");

        assert_eq!(decrypted, plaintext);

        // CLI sends reply
        let reply = b"Hello Browser!";
        let reply_envelope = cli_session.encrypt(reply, &cli_identity);
        assert_eq!(reply_envelope.message_type, 1); // Should be Normal now

        // Browser decrypts reply (simulate with vodozemac API)
        let reply_ciphertext = BASE64.decode(&reply_envelope.ciphertext).unwrap();
        let reply_message = vodozemac::olm::Message::try_from(reply_ciphertext.as_slice()).unwrap();
        let reply_decrypted = browser_session.decrypt(&OlmMessage::Normal(reply_message)).unwrap();
        assert_eq!(reply_decrypted, reply);
    }

    #[test]
    fn test_session_pickle() {
        let mut account = OlmAccount::new();
        account.generate_one_time_keys(1);

        // Create a session for pickling
        let identity = account.curve25519_key();
        let otk = account.get_one_time_key().unwrap();

        let other_account = OlmAccount::new();

        let identity_key = Curve25519PublicKey::from_base64(&identity).unwrap();
        let one_time_key = Curve25519PublicKey::from_base64(&otk).unwrap();
        let session = other_account.inner.create_outbound_session(
            SessionConfig::version_2(),
            identity_key,
            one_time_key,
        );

        let wrapped = OlmSession {
            inner: session,
            peer_curve25519: identity.clone(),
        };

        let session_id = wrapped.session_id();
        let pickle = wrapped.pickle();

        let restored = OlmSession::from_pickle(&pickle, identity).expect("Failed to restore");
        assert_eq!(session_id, restored.session_id());
    }
}
