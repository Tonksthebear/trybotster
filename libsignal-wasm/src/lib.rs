//! WebAssembly bindings for Signal Protocol E2E encryption.
//!
//! This crate provides browser-side Signal Protocol encryption for
//! secure communication with the CLI.
//!
//! # Protocol Flow
//!
//! ```text
//! Browser (this crate)                        CLI
//! ─────────────────────────────────────────────────────────
//!                                   1. Generate PreKeyBundle
//!                                   2. Display QR code
//!
//! 3. Scan QR, parse PreKeyBundleData
//! 4. SignalSession.create(bundle)
//! 5. session.encrypt(handshake)
//! 6. Send PreKeySignalMessage ─────────────►
//!
//!                                   7. Decrypt, create session
//!                                   8. Session established
//!
//!    ◄──── Encrypted messages (SignalMessage) ────►
//! ```

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use wasm_bindgen::prelude::*;

mod stores;

/// Get current time in WASM by using JavaScript's Date.now().
///
/// Returns a std::time::SystemTime that libsignal can use.
fn wasm_now() -> SystemTime {
    let millis = js_sys::Date::now() as u64;
    UNIX_EPOCH + Duration::from_millis(millis)
}

use stores::BrowserSignalStore;

/// Signal Protocol errors for WASM bindings.
#[derive(Error, Debug)]
pub enum SignalError {
    #[error("Failed to create session: {0}")]
    SessionCreation(String),
    #[error("Failed to encrypt: {0}")]
    Encryption(String),
    #[error("Failed to decrypt: {0}")]
    Decryption(String),
    #[error("Invalid bundle: {0}")]
    InvalidBundle(String),
    #[error("Invalid message: {0}")]
    InvalidMessage(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<SignalError> for JsValue {
    fn from(err: SignalError) -> Self {
        JsValue::from_str(&err.to_string())
    }
}

/// Protocol version for Signal messages.
const SIGNAL_PROTOCOL_VERSION: u8 = 4;

/// Browser device ID (CLI = 1, browsers start at 2).
const BROWSER_DEVICE_ID: u32 = 2;

/// Message type constants.
const MSG_TYPE_PREKEY: u8 = 1;
const MSG_TYPE_SIGNAL: u8 = 2;
const MSG_TYPE_SENDER_KEY: u8 = 3;

/// PreKeyBundle data received from CLI (via QR code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreKeyBundleData {
    pub version: u8,
    pub hub_id: String,
    pub registration_id: u32,
    pub device_id: u32,
    pub identity_key: String,
    pub signed_prekey_id: u32,
    pub signed_prekey: String,
    pub signed_prekey_signature: String,
    pub prekey_id: Option<u32>,
    pub prekey: Option<String>,
    pub kyber_prekey_id: u32,
    pub kyber_prekey: String,
    pub kyber_prekey_signature: String,
}

/// Encrypted Signal message envelope (minimal format).
///
/// Uses short keys to minimize wire size:
/// - t: message_type (1=PreKey, 2=Signal, 3=SenderKey)
/// - c: ciphertext (base64)
/// - s: sender_identity (base64)
/// - d: device_id
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalEnvelope {
    /// Message type: 1=PreKey, 2=Signal, 3=SenderKey
    #[serde(rename = "t")]
    pub message_type: u8,
    /// Base64-encoded ciphertext
    #[serde(rename = "c")]
    pub ciphertext: String,
    /// Sender's identity public key (base64)
    #[serde(rename = "s")]
    pub sender_identity: String,
    /// Sender's device ID (CLI=1, browser=2)
    #[serde(rename = "d")]
    pub device_id: u32,
}

/// A Signal Protocol session for the browser.
///
/// Handles encryption/decryption using X3DH and Double Ratchet.
#[wasm_bindgen]
pub struct SignalSession {
    store: BrowserSignalStore,
    cli_address: libsignal_protocol::ProtocolAddress,
    hub_id: String,
}

#[wasm_bindgen]
impl SignalSession {
    /// Create a new session from a PreKeyBundle JSON string.
    ///
    /// This performs X3DH key agreement and creates the initial
    /// Double Ratchet session.
    #[wasm_bindgen(constructor)]
    pub async fn new(prekey_bundle_json: &str) -> Result<SignalSession, JsValue> {
        use libsignal_protocol::{
            process_prekey_bundle, DeviceId, IdentityKey, PreKeyBundle, PreKeyId,
            SignedPreKeyId, KyberPreKeyId, PublicKey,
        };

        let bundle: PreKeyBundleData = serde_json::from_str(prekey_bundle_json)
            .map_err(|e| SignalError::InvalidBundle(e.to_string()))?;

        // Parse CLI's public keys
        let identity_key_bytes = BASE64
            .decode(&bundle.identity_key)
            .map_err(|e| SignalError::InvalidBundle(format!("identity_key: {e}")))?;
        let identity_key = IdentityKey::decode(&identity_key_bytes)
            .map_err(|e| SignalError::InvalidBundle(format!("identity_key decode: {e}")))?;

        let signed_prekey_bytes = BASE64
            .decode(&bundle.signed_prekey)
            .map_err(|e| SignalError::InvalidBundle(format!("signed_prekey: {e}")))?;
        let signed_prekey = PublicKey::deserialize(&signed_prekey_bytes)
            .map_err(|e| SignalError::InvalidBundle(format!("signed_prekey decode: {e}")))?;

        let signed_prekey_signature = BASE64
            .decode(&bundle.signed_prekey_signature)
            .map_err(|e| SignalError::InvalidBundle(format!("signature: {e}")))?;

        // Parse one-time PreKey if present
        let prekey = if let (Some(id), Some(key)) = (bundle.prekey_id, &bundle.prekey) {
            let key_bytes = BASE64
                .decode(key)
                .map_err(|e| SignalError::InvalidBundle(format!("prekey: {e}")))?;
            let pubkey = PublicKey::deserialize(&key_bytes)
                .map_err(|e| SignalError::InvalidBundle(format!("prekey decode: {e}")))?;
            Some((PreKeyId::from(id), pubkey))
        } else {
            None
        };

        // Parse Kyber PreKey
        let kyber_prekey_bytes = BASE64
            .decode(&bundle.kyber_prekey)
            .map_err(|e| SignalError::InvalidBundle(format!("kyber_prekey: {e}")))?;
        let kyber_prekey = libsignal_protocol::kem::PublicKey::deserialize(&kyber_prekey_bytes)
            .map_err(|e| SignalError::InvalidBundle(format!("kyber_prekey decode: {e}")))?;

        let kyber_prekey_signature = BASE64
            .decode(&bundle.kyber_prekey_signature)
            .map_err(|e| SignalError::InvalidBundle(format!("kyber_signature: {e}")))?;

        // Build PreKeyBundle (10 args in latest libsignal)
        let prekey_bundle = PreKeyBundle::new(
            bundle.registration_id,
            DeviceId::new(bundle.device_id as u8).expect("valid device ID"),
            prekey, // Option<(PreKeyId, PublicKey)>
            SignedPreKeyId::from(bundle.signed_prekey_id),
            signed_prekey,
            signed_prekey_signature, // Vec<u8>
            KyberPreKeyId::from(bundle.kyber_prekey_id),
            kyber_prekey,
            kyber_prekey_signature, // Vec<u8>
            identity_key,
        )
        .map_err(|e| SignalError::InvalidBundle(format!("bundle construction: {e}")))?;

        // Create our browser-side store
        let store = BrowserSignalStore::new()
            .await
            .map_err(|e| SignalError::SessionCreation(e.to_string()))?;

        // CLI's address (who we're talking to)
        let cli_address = libsignal_protocol::ProtocolAddress::new(
            bundle.identity_key.clone(),
            DeviceId::new(bundle.device_id as u8).expect("valid device ID"),
        );

        // Process the PreKeyBundle to establish session
        let mut identity_store = store.clone();
        let mut session_store = store.clone();

        process_prekey_bundle(
            &cli_address,
            &mut session_store,
            &mut identity_store,
            &prekey_bundle,
            wasm_now(),
            &mut rand::rngs::StdRng::from_os_rng(),
        )
        .await
        .map_err(|e| SignalError::SessionCreation(format!("process_prekey_bundle: {e}")))?;

        Ok(SignalSession {
            store,
            cli_address,
            hub_id: bundle.hub_id,
        })
    }

    /// Encrypt a message for the CLI.
    ///
    /// Takes a JSON string and returns a SignalEnvelope JSON string.
    #[wasm_bindgen]
    pub async fn encrypt(&mut self, message_json: &str) -> Result<String, JsValue> {
        use libsignal_protocol::{message_encrypt, CiphertextMessageType};

        let plaintext = message_json.as_bytes();

        let mut session_store = self.store.clone();
        let mut identity_store = self.store.clone();

        let ciphertext = message_encrypt(
            plaintext,
            &self.cli_address,
            &mut session_store,
            &mut identity_store,
            wasm_now(),
            &mut rand::rngs::StdRng::from_os_rng(),
        )
        .await
        .map_err(|e| SignalError::Encryption(e.to_string()))?;

        let message_type = match ciphertext.message_type() {
            CiphertextMessageType::PreKey => MSG_TYPE_PREKEY,
            CiphertextMessageType::Whisper => MSG_TYPE_SIGNAL,
            CiphertextMessageType::SenderKey => MSG_TYPE_SENDER_KEY,
            _ => MSG_TYPE_SIGNAL,
        };

        let identity = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| SignalError::Encryption(format!("get identity: {e}")))?;
        let registration_id = self
            .store
            .get_local_registration_id()
            .await
            .map_err(|e| SignalError::Encryption(format!("get reg id: {e}")))?;

        let envelope = SignalEnvelope {
            message_type,
            ciphertext: BASE64.encode(ciphertext.serialize()),
            sender_identity: BASE64.encode(identity.public_key().serialize()),
            device_id: BROWSER_DEVICE_ID,
        };

        serde_json::to_string(&envelope).map_err(|e| SignalError::Serialization(e.to_string()).into())
    }

    /// Decrypt a message from the CLI.
    ///
    /// Takes a SignalEnvelope JSON string and returns the decrypted JSON string.
    #[wasm_bindgen]
    pub async fn decrypt(&mut self, envelope_json: &str) -> Result<String, JsValue> {
        use libsignal_protocol::{
            message_decrypt_signal, PreKeySignalMessage, SignalMessage,
        };

        let envelope: SignalEnvelope = serde_json::from_str(envelope_json)
            .map_err(|e| SignalError::InvalidMessage(e.to_string()))?;

        let ciphertext = BASE64
            .decode(&envelope.ciphertext)
            .map_err(|e| SignalError::InvalidMessage(format!("ciphertext: {e}")))?;

        // CLI's address (the sender)
        let sender_address = libsignal_protocol::ProtocolAddress::new(
            envelope.sender_identity.clone(),
            libsignal_protocol::DeviceId::new(envelope.device_id as u8).expect("valid device ID"),
        );

        let plaintext = match envelope.message_type {
            MSG_TYPE_PREKEY => {
                // CLI shouldn't send us PreKey messages - we initiate
                // But handle it anyway for completeness
                let prekey_msg = PreKeySignalMessage::try_from(ciphertext.as_slice())
                    .map_err(|e| SignalError::InvalidMessage(format!("prekey parse: {e}")))?;

                let mut session_store = self.store.clone();
                let mut identity_store = self.store.clone();
                let mut prekey_store = self.store.clone();
                let signed_prekey_store = self.store.clone();
                let mut kyber_store = self.store.clone();

                libsignal_protocol::message_decrypt_prekey(
                    &prekey_msg,
                    &sender_address,
                    &mut session_store,
                    &mut identity_store,
                    &mut prekey_store,
                    &signed_prekey_store,
                    &mut kyber_store,
                    &mut rand::rngs::StdRng::from_os_rng(),
                )
                .await
                .map_err(|e| SignalError::Decryption(format!("prekey decrypt: {e}")))?
            }
            MSG_TYPE_SIGNAL => {
                let signal_msg = SignalMessage::try_from(ciphertext.as_slice())
                    .map_err(|e| SignalError::InvalidMessage(format!("signal parse: {e}")))?;

                let mut session_store = self.store.clone();
                let mut identity_store = self.store.clone();

                message_decrypt_signal(
                    &signal_msg,
                    &sender_address,
                    &mut session_store,
                    &mut identity_store,
                    &mut rand::rngs::StdRng::from_os_rng(),
                )
                .await
                .map_err(|e| SignalError::Decryption(format!("signal decrypt: {e}")))?
            }
            MSG_TYPE_SENDER_KEY => {
                // SenderKey for group broadcasts from CLI
                self.sender_key_decrypt_inner(&ciphertext, &sender_address)
                    .await?
            }
            _ => {
                return Err(SignalError::InvalidMessage(format!(
                    "unknown type: {}",
                    envelope.message_type
                ))
                .into());
            }
        };

        String::from_utf8(plaintext)
            .map_err(|e| SignalError::Decryption(format!("utf8: {e}")).into())
    }

    /// Process a SenderKey distribution message from the CLI.
    ///
    /// Call this when CLI sends you a distribution message (via individual session).
    #[wasm_bindgen]
    pub async fn process_sender_key_distribution(&mut self, distribution_b64: &str) -> Result<(), JsValue> {
        use libsignal_protocol::{process_sender_key_distribution_message, SenderKeyDistributionMessage};

        let distribution_bytes = BASE64
            .decode(distribution_b64)
            .map_err(|e| SignalError::InvalidMessage(format!("distribution: {e}")))?;

        let distribution = SenderKeyDistributionMessage::try_from(distribution_bytes.as_slice())
            .map_err(|e| SignalError::InvalidMessage(format!("distribution parse: {e}")))?;

        process_sender_key_distribution_message(
            &self.cli_address,
            &distribution,
            &mut self.store,
        )
        .await
        .map_err(|e| SignalError::SessionCreation(format!("sender key: {e}")))?;

        Ok(())
    }

    /// Get our identity public key (base64).
    #[wasm_bindgen]
    pub async fn get_identity_key(&self) -> Result<String, JsValue> {
        let identity = self
            .store
            .get_identity_key_pair()
            .await
            .map_err(|e| JsValue::from_str(&format!("get identity: {e}")))?;
        Ok(BASE64.encode(identity.public_key().serialize()))
    }

    /// Get the hub ID this session is connected to.
    #[wasm_bindgen]
    pub fn get_hub_id(&self) -> String {
        self.hub_id.clone()
    }

    /// Pickle (serialize) the session for IndexedDB storage.
    #[wasm_bindgen]
    pub fn pickle(&self) -> Result<String, JsValue> {
        self.store
            .pickle(&self.cli_address, &self.hub_id)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Restore a session from a pickled string.
    #[wasm_bindgen]
    pub fn from_pickle(pickle: &str) -> Result<SignalSession, JsValue> {
        let (store, cli_address, hub_id) = BrowserSignalStore::from_pickle(pickle)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        Ok(SignalSession {
            store,
            cli_address,
            hub_id,
        })
    }

    /// Decrypt a SenderKey message (internal helper).
    async fn sender_key_decrypt_inner(
        &mut self,
        ciphertext: &[u8],
        sender: &libsignal_protocol::ProtocolAddress,
    ) -> Result<Vec<u8>, SignalError> {
        use libsignal_protocol::group_decrypt;

        group_decrypt(
            ciphertext,
            &mut self.store,
            sender,
        )
        .await
        .map_err(|e| SignalError::Decryption(format!("senderkey: {e}")))
    }
}

/// Initialize the WASM module.
#[wasm_bindgen(start)]
pub fn init() {
    // Set up panic hook for better error messages in browser console
    console_error_panic_hook::set_once();
}

/// Test function to verify WASM loads correctly.
#[wasm_bindgen]
pub fn ping() -> String {
    "libsignal-wasm loaded (Signal Protocol v4)".to_string()
}
