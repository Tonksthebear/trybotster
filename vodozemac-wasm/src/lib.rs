//! WebAssembly bindings for vodozemac Olm encryption.
//!
//! This crate provides WASM bindings for vodozemac's Olm implementation,
//! enabling end-to-end encryption in the browser with battle-tested,
//! audited cryptography.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wasm_bindgen::prelude::*;

use vodozemac::olm::{
    Account as VodozemacAccount, AccountPickle, InboundCreationResult, OlmMessage,
    Session as VodozemacSession, SessionConfig, SessionPickle,
};
use vodozemac::{Curve25519PublicKey, Ed25519PublicKey};

/// Error types for WASM bindings
#[derive(Error, Debug)]
pub enum OlmError {
    #[error("Failed to create session: {0}")]
    SessionCreation(String),
    #[error("Failed to decrypt: {0}")]
    Decryption(String),
    #[error("Failed to unpickle: {0}")]
    Unpickle(String),
    #[error("Invalid key format: {0}")]
    InvalidKey(String),
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),
}

impl From<OlmError> for JsValue {
    fn from(err: OlmError) -> Self {
        JsValue::from_str(&err.to_string())
    }
}

// ============================================================================
// Identity Keys
// ============================================================================

/// Identity keys for an Olm account (Ed25519 + Curve25519)
#[wasm_bindgen]
#[derive(Clone)]
pub struct IdentityKeys {
    ed25519: String,
    curve25519: String,
}

#[wasm_bindgen]
impl IdentityKeys {
    /// Get the Ed25519 signing key (base64)
    #[wasm_bindgen(getter)]
    pub fn ed25519(&self) -> String {
        self.ed25519.clone()
    }

    /// Get the Curve25519 encryption key (base64)
    #[wasm_bindgen(getter)]
    pub fn curve25519(&self) -> String {
        self.curve25519.clone()
    }
}

// ============================================================================
// One-Time Key
// ============================================================================

/// A one-time key with its ID
#[wasm_bindgen]
#[derive(Clone, Serialize, Deserialize)]
pub struct OneTimeKey {
    key_id: String,
    key: String,
}

#[wasm_bindgen]
impl OneTimeKey {
    /// Get the key ID
    #[wasm_bindgen(getter)]
    pub fn key_id(&self) -> String {
        self.key_id.clone()
    }

    /// Get the key (base64)
    #[wasm_bindgen(getter)]
    pub fn key(&self) -> String {
        self.key.clone()
    }
}

// ============================================================================
// Encrypted Message
// ============================================================================

/// An encrypted Olm message
#[wasm_bindgen]
#[derive(Clone)]
pub struct EncryptedMessage {
    message_type: u8,
    ciphertext: String,
}

#[wasm_bindgen]
impl EncryptedMessage {
    /// Create a new EncryptedMessage (for receiving messages)
    #[wasm_bindgen(constructor)]
    pub fn new(message_type: u8, ciphertext: String) -> Self {
        Self {
            message_type,
            ciphertext,
        }
    }

    /// Get the message type (0 = PreKey, 1 = Normal)
    #[wasm_bindgen(getter)]
    pub fn message_type(&self) -> u8 {
        self.message_type
    }

    /// Get the ciphertext (base64)
    #[wasm_bindgen(getter)]
    pub fn ciphertext(&self) -> String {
        self.ciphertext.clone()
    }
}

impl EncryptedMessage {
    fn from_olm_message(msg: OlmMessage) -> Self {
        match msg {
            OlmMessage::PreKey(m) => Self {
                message_type: 0,
                ciphertext: BASE64.encode(m.to_bytes()),
            },
            OlmMessage::Normal(m) => Self {
                message_type: 1,
                ciphertext: BASE64.encode(m.to_bytes()),
            },
        }
    }

    fn to_olm_message(&self) -> Result<OlmMessage, OlmError> {
        let bytes = BASE64
            .decode(&self.ciphertext)
            .map_err(|e| OlmError::InvalidMessage(e.to_string()))?;

        match self.message_type {
            0 => {
                let msg = vodozemac::olm::PreKeyMessage::try_from(bytes.as_slice())
                    .map_err(|e| OlmError::InvalidMessage(e.to_string()))?;
                Ok(OlmMessage::PreKey(msg))
            }
            1 => {
                let msg = vodozemac::olm::Message::try_from(bytes.as_slice())
                    .map_err(|e| OlmError::InvalidMessage(e.to_string()))?;
                Ok(OlmMessage::Normal(msg))
            }
            _ => Err(OlmError::InvalidMessage(format!(
                "Unknown message type: {}",
                self.message_type
            ))),
        }
    }
}

// ============================================================================
// Session Creation Result
// ============================================================================

/// Result of creating an inbound session
#[wasm_bindgen]
pub struct SessionCreationResult {
    session: Option<Session>,
    plaintext: Vec<u8>,
}

#[wasm_bindgen]
impl SessionCreationResult {
    /// Take the created session (can only be called once)
    #[wasm_bindgen]
    pub fn take_session(&mut self) -> Result<Session, JsValue> {
        self.session
            .take()
            .ok_or_else(|| JsValue::from_str("Session already taken"))
    }

    /// Get the decrypted plaintext as a string (UTF-8)
    #[wasm_bindgen]
    pub fn plaintext_string(&self) -> Result<String, JsValue> {
        String::from_utf8(self.plaintext.clone())
            .map_err(|e| JsValue::from_str(&format!("Invalid UTF-8: {}", e)))
    }

    /// Get the decrypted plaintext as bytes
    #[wasm_bindgen]
    pub fn plaintext_bytes(&self) -> Vec<u8> {
        self.plaintext.clone()
    }
}

// ============================================================================
// Olm Account
// ============================================================================

/// An Olm account holding identity keys and one-time keys
#[wasm_bindgen]
pub struct Account {
    inner: VodozemacAccount,
}

#[wasm_bindgen]
impl Account {
    /// Create a new Olm account with fresh keys
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: VodozemacAccount::new(),
        }
    }

    /// Get the account's identity keys
    #[wasm_bindgen]
    pub fn identity_keys(&self) -> IdentityKeys {
        let keys = self.inner.identity_keys();
        IdentityKeys {
            ed25519: keys.ed25519.to_base64(),
            curve25519: keys.curve25519.to_base64(),
        }
    }

    /// Generate new one-time keys
    #[wasm_bindgen]
    pub fn generate_one_time_keys(&mut self, count: usize) {
        self.inner.generate_one_time_keys(count);
    }

    /// Get unpublished one-time keys
    #[wasm_bindgen]
    pub fn one_time_keys(&self) -> Result<JsValue, JsValue> {
        let keys = self.inner.one_time_keys();
        let result: Vec<OneTimeKey> = keys
            .into_iter()
            .map(|(id, key)| OneTimeKey {
                key_id: id.to_base64(),
                key: key.to_base64(),
            })
            .collect();
        serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Mark one-time keys as published
    #[wasm_bindgen]
    pub fn mark_keys_as_published(&mut self) {
        self.inner.mark_keys_as_published();
    }

    /// Get the maximum number of one-time keys the account can hold
    #[wasm_bindgen]
    pub fn max_one_time_keys(&self) -> usize {
        self.inner.max_number_of_one_time_keys()
    }

    /// Create an outbound session to a peer
    ///
    /// # Arguments
    /// * `their_identity_key` - Peer's Curve25519 identity key (base64)
    /// * `their_one_time_key` - Peer's one-time key (base64)
    #[wasm_bindgen]
    pub fn create_outbound_session(
        &mut self,
        their_identity_key: &str,
        their_one_time_key: &str,
    ) -> Result<Session, JsValue> {
        let identity_key = Curve25519PublicKey::from_base64(their_identity_key)
            .map_err(|e| OlmError::InvalidKey(e.to_string()))?;
        let one_time_key = Curve25519PublicKey::from_base64(their_one_time_key)
            .map_err(|e| OlmError::InvalidKey(e.to_string()))?;

        let session = self
            .inner
            .create_outbound_session(SessionConfig::version_2(), identity_key, one_time_key);

        Ok(Session { inner: session })
    }

    /// Create an inbound session from a PreKey message
    ///
    /// # Arguments
    /// * `their_identity_key` - Sender's Curve25519 identity key (base64)
    /// * `message` - The PreKey message
    #[wasm_bindgen]
    pub fn create_inbound_session(
        &mut self,
        their_identity_key: &str,
        message: &EncryptedMessage,
    ) -> Result<SessionCreationResult, JsValue> {
        if message.message_type != 0 {
            return Err(OlmError::InvalidMessage(
                "Expected PreKey message (type 0)".to_string(),
            )
            .into());
        }

        let identity_key = Curve25519PublicKey::from_base64(their_identity_key)
            .map_err(|e| OlmError::InvalidKey(e.to_string()))?;

        let olm_message = message.to_olm_message()?;
        let prekey_message = match olm_message {
            OlmMessage::PreKey(m) => m,
            _ => unreachable!(),
        };

        let InboundCreationResult {
            session,
            plaintext,
        } = self
            .inner
            .create_inbound_session(identity_key, &prekey_message)
            .map_err(|e| OlmError::SessionCreation(e.to_string()))?;

        Ok(SessionCreationResult {
            session: Some(Session { inner: session }),
            plaintext,
        })
    }

    /// Sign a message with the account's Ed25519 key
    #[wasm_bindgen]
    pub fn sign(&self, message: &str) -> String {
        let signature = self.inner.sign(message);
        signature.to_base64()
    }

    /// Pickle (serialize) the account for storage
    #[wasm_bindgen]
    pub fn pickle(&self) -> String {
        let pickle = self.inner.pickle();
        serde_json::to_string(&pickle).expect("Failed to serialize pickle")
    }

    /// Unpickle (deserialize) an account from storage
    #[wasm_bindgen]
    pub fn from_pickle(pickle: &str) -> Result<Account, JsValue> {
        let account_pickle: AccountPickle =
            serde_json::from_str(pickle).map_err(|e| OlmError::Unpickle(e.to_string()))?;
        let account = VodozemacAccount::from_pickle(account_pickle);
        Ok(Account { inner: account })
    }
}

impl Default for Account {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Olm Session
// ============================================================================

/// An Olm session for encrypting/decrypting messages
#[wasm_bindgen]
pub struct Session {
    inner: VodozemacSession,
}

#[wasm_bindgen]
impl Session {
    /// Get the session ID
    #[wasm_bindgen]
    pub fn session_id(&self) -> String {
        self.inner.session_id()
    }

    /// Check if this session has received a message
    #[wasm_bindgen]
    pub fn has_received_message(&self) -> bool {
        self.inner.has_received_message()
    }

    /// Encrypt a message
    #[wasm_bindgen]
    pub fn encrypt(&mut self, plaintext: &str) -> EncryptedMessage {
        let message = self.inner.encrypt(plaintext);
        EncryptedMessage::from_olm_message(message)
    }

    /// Decrypt a message
    #[wasm_bindgen]
    pub fn decrypt(&mut self, message: &EncryptedMessage) -> Result<String, JsValue> {
        let olm_message = message.to_olm_message()?;
        let plaintext = self
            .inner
            .decrypt(&olm_message)
            .map_err(|e| OlmError::Decryption(e.to_string()))?;
        String::from_utf8(plaintext).map_err(|e| JsValue::from_str(&format!("Invalid UTF-8: {}", e)))
    }

    /// Pickle (serialize) the session for storage
    #[wasm_bindgen]
    pub fn pickle(&self) -> String {
        let pickle = self.inner.pickle();
        serde_json::to_string(&pickle).expect("Failed to serialize pickle")
    }

    /// Unpickle (deserialize) a session from storage
    #[wasm_bindgen]
    pub fn from_pickle(pickle: &str) -> Result<Session, JsValue> {
        let session_pickle: SessionPickle =
            serde_json::from_str(pickle).map_err(|e| OlmError::Unpickle(e.to_string()))?;
        let session = VodozemacSession::from_pickle(session_pickle);
        Ok(Session { inner: session })
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Initialize the WASM module (call once at startup)
#[wasm_bindgen(start)]
pub fn init() {
    // Future: Add console_error_panic_hook for better error messages
}

/// Verify an Ed25519 signature
#[wasm_bindgen]
pub fn verify_signature(
    public_key: &str,
    message: &str,
    signature: &str,
) -> Result<bool, JsValue> {
    let key = Ed25519PublicKey::from_base64(public_key)
        .map_err(|e| OlmError::InvalidKey(e.to_string()))?;
    let sig = vodozemac::Ed25519Signature::from_base64(signature)
        .map_err(|e| OlmError::InvalidKey(e.to_string()))?;

    Ok(key.verify(message.as_bytes(), &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_creation() {
        let account = Account::new();
        let keys = account.identity_keys();
        assert!(!keys.ed25519().is_empty());
        assert!(!keys.curve25519().is_empty());
    }

    #[test]
    fn test_one_time_keys() {
        let mut account = Account::new();
        account.generate_one_time_keys(5);
        // Keys should be generated
        account.mark_keys_as_published();
    }

    #[test]
    fn test_sign_and_verify() {
        let account = Account::new();
        let message = "Hello, World!";
        let signature = account.sign(message);
        let keys = account.identity_keys();

        let result = verify_signature(&keys.ed25519(), message, &signature);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_account_pickle() {
        let account = Account::new();
        let keys1 = account.identity_keys();

        let pickle = account.pickle();
        let restored = Account::from_pickle(&pickle).unwrap();
        let keys2 = restored.identity_keys();

        assert_eq!(keys1.ed25519(), keys2.ed25519());
        assert_eq!(keys1.curve25519(), keys2.curve25519());
    }

    #[test]
    fn test_session_establishment() {
        // Alice creates an account
        let mut alice = Account::new();
        alice.generate_one_time_keys(1);

        // Bob creates an account
        let mut bob = Account::new();

        // Bob gets Alice's identity key and one-time key
        let alice_identity = alice.identity_keys().curve25519();

        // Get Alice's one-time key (we need to access inner for this test)
        let alice_otk = {
            let keys = alice.inner.one_time_keys();
            let (_, key) = keys.into_iter().next().unwrap();
            key.to_base64()
        };

        // Bob creates outbound session to Alice
        let mut bob_session = bob
            .create_outbound_session(&alice_identity, &alice_otk)
            .unwrap();

        // Bob encrypts a message (PreKey message)
        let message = bob_session.encrypt("Hello Alice!");
        assert_eq!(message.message_type(), 0); // PreKey

        // Alice creates inbound session from Bob's message
        let bob_identity = bob.identity_keys().curve25519();
        let mut result = alice.create_inbound_session(&bob_identity, &message).unwrap();

        assert_eq!(result.plaintext_string().unwrap(), "Hello Alice!");

        // Get Alice's session (takes ownership)
        let mut alice_session = result.take_session().unwrap();

        // Alice can now encrypt back
        let reply = alice_session.encrypt("Hello Bob!");
        assert_eq!(reply.message_type(), 1); // Normal message now

        // Bob decrypts
        let decrypted = bob_session.decrypt(&reply).unwrap();
        assert_eq!(decrypted, "Hello Bob!");
    }

    #[test]
    fn test_session_pickle() {
        let mut alice = Account::new();
        alice.generate_one_time_keys(1);
        let mut bob = Account::new();

        let alice_identity = alice.identity_keys().curve25519();
        let alice_otk = {
            let keys = alice.inner.one_time_keys();
            let (_, key) = keys.into_iter().next().unwrap();
            key.to_base64()
        };

        let session = bob
            .create_outbound_session(&alice_identity, &alice_otk)
            .unwrap();

        let session_id = session.session_id();
        let pickle = session.pickle();

        let restored = Session::from_pickle(&pickle).unwrap();
        assert_eq!(restored.session_id(), session_id);
    }
}
