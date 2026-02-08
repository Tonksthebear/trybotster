use wasm_bindgen::prelude::*;

use vodozemac::olm::{
    Account, InboundCreationResult, OlmMessage, Session, SessionConfig,
};
use vodozemac::{Curve25519PublicKey, KeyId};

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// VodozemacAccount
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct VodozemacAccount {
    inner: Account,
}

#[wasm_bindgen]
impl VodozemacAccount {
    /// Create a brand-new Olm Account with random identity keys.
    pub fn create() -> Self {
        Self {
            inner: Account::new(),
        }
    }

    /// Restore an Account from an encrypted pickle string.
    /// `pickle_key` must be exactly 32 bytes.
    #[wasm_bindgen(js_name = "fromPickle")]
    pub fn from_pickle(pickle: &str, pickle_key: &[u8]) -> Result<VodozemacAccount, JsError> {
        let key: &[u8; 32] = pickle_key
            .try_into()
            .map_err(|_| JsError::new("pickle_key must be exactly 32 bytes"))?;

        let account_pickle = vodozemac::olm::AccountPickle::from_encrypted(pickle, key)
            .map_err(|e| JsError::new(&format!("unpickle failed: {e}")))?;

        Ok(Self {
            inner: Account::from_pickle(account_pickle),
        })
    }

    /// Serialize and encrypt the Account into a pickle string.
    /// `pickle_key` must be exactly 32 bytes.
    pub fn pickle(&self, pickle_key: &[u8]) -> Result<String, JsError> {
        let key: &[u8; 32] = pickle_key
            .try_into()
            .map_err(|_| JsError::new("pickle_key must be exactly 32 bytes"))?;

        Ok(self.inner.pickle().encrypt(key))
    }

    /// Return the Curve25519 identity key as unpadded base64.
    #[wasm_bindgen(js_name = "curve25519Key")]
    pub fn curve25519_key(&self) -> String {
        self.inner.curve25519_key().to_base64()
    }

    /// Return the Ed25519 identity key as unpadded base64.
    #[wasm_bindgen(js_name = "ed25519Key")]
    pub fn ed25519_key(&self) -> String {
        self.inner.ed25519_key().to_base64()
    }

    /// Sign a message with the Ed25519 key. Returns unpadded base64 signature.
    pub fn sign(&self, message: &[u8]) -> String {
        self.inner.sign(message).to_base64()
    }

    /// Create an outbound Olm session using the recipient's identity key and
    /// one-time key (both unpadded base64).
    #[wasm_bindgen(js_name = "createOutboundSession")]
    pub fn create_outbound_session(
        &mut self,
        identity_key: &str,
        one_time_key: &str,
    ) -> Result<VodozemacSession, JsError> {
        let id_key = Curve25519PublicKey::from_base64(identity_key)
            .map_err(|e| JsError::new(&format!("bad identity_key: {e}")))?;

        let otk = Curve25519PublicKey::from_base64(one_time_key)
            .map_err(|e| JsError::new(&format!("bad one_time_key: {e}")))?;

        let session = self
            .inner
            .create_outbound_session(SessionConfig::version_2(), id_key, otk);

        Ok(VodozemacSession { inner: session })
    }

    /// Create an inbound session from a pre-key message.
    ///
    /// `identity_key` — sender's Curve25519 key (unpadded base64).
    /// `prekey_message` — raw bytes of the pre-key message.
    ///
    /// Returns a JS object `{ session: VodozemacSession, plaintext: Uint8Array }`.
    #[wasm_bindgen(js_name = "createInboundSession")]
    pub fn create_inbound_session(
        &mut self,
        identity_key: &str,
        prekey_message: &[u8],
    ) -> Result<JsValue, JsError> {
        let id_key = Curve25519PublicKey::from_base64(identity_key)
            .map_err(|e| JsError::new(&format!("bad identity_key: {e}")))?;

        let prekey_msg = vodozemac::olm::PreKeyMessage::from_bytes(prekey_message)
            .map_err(|e| JsError::new(&format!("bad prekey_message: {e}")))?;

        let InboundCreationResult { session, plaintext } = self
            .inner
            .create_inbound_session(id_key, &prekey_msg)
            .map_err(|e| JsError::new(&format!("inbound session failed: {e}")))?;

        // Build the JS return value: { session, plaintext }
        let obj = js_sys::Object::new();
        let voz_session = VodozemacSession { inner: session };

        js_sys::Reflect::set(&obj, &"session".into(), &voz_session.into())
            .map_err(|_| JsError::new("Reflect::set session"))?;
        js_sys::Reflect::set(
            &obj,
            &"plaintext".into(),
            &js_sys::Uint8Array::from(plaintext.as_slice()).into(),
        )
        .map_err(|_| JsError::new("Reflect::set plaintext"))?;

        Ok(obj.into())
    }

    /// Generate `count` new one-time keys.
    #[wasm_bindgen(js_name = "generateOneTimeKeys")]
    pub fn generate_one_time_keys(&mut self, count: u32) {
        self.inner.generate_one_time_keys(count as usize);
    }

    /// Return unpublished one-time keys as a JS object `{ keyId: base64Key, ... }`.
    #[wasm_bindgen(js_name = "oneTimeKeys")]
    pub fn one_time_keys(&self) -> Result<JsValue, JsError> {
        let keys: HashMap<KeyId, Curve25519PublicKey> = self.inner.one_time_keys();
        let obj = js_sys::Object::new();

        for (key_id, curve_key) in keys {
            let id_str: String = key_id.to_base64();
            let key_b64 = curve_key.to_base64();
            js_sys::Reflect::set(&obj, &id_str.into(), &key_b64.into())
                .map_err(|_| JsError::new("Reflect::set one_time_key"))?;
        }

        Ok(obj.into())
    }

    /// Mark all one-time keys as published.
    #[wasm_bindgen(js_name = "markKeysAsPublished")]
    pub fn mark_keys_as_published(&mut self) {
        self.inner.mark_keys_as_published();
    }
}

// ---------------------------------------------------------------------------
// VodozemacSession
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct VodozemacSession {
    inner: Session,
}

#[wasm_bindgen]
impl VodozemacSession {
    /// Restore a Session from an encrypted pickle string.
    /// `pickle_key` must be exactly 32 bytes.
    #[wasm_bindgen(js_name = "fromPickle")]
    pub fn from_pickle(pickle: &str, pickle_key: &[u8]) -> Result<VodozemacSession, JsError> {
        let key: &[u8; 32] = pickle_key
            .try_into()
            .map_err(|_| JsError::new("pickle_key must be exactly 32 bytes"))?;

        let session_pickle = vodozemac::olm::SessionPickle::from_encrypted(pickle, key)
            .map_err(|e| JsError::new(&format!("unpickle failed: {e}")))?;

        Ok(Self {
            inner: Session::from_pickle(session_pickle),
        })
    }

    /// Serialize and encrypt the Session into a pickle string.
    /// `pickle_key` must be exactly 32 bytes.
    pub fn pickle(&self, pickle_key: &[u8]) -> Result<String, JsError> {
        let key: &[u8; 32] = pickle_key
            .try_into()
            .map_err(|_| JsError::new("pickle_key must be exactly 32 bytes"))?;

        Ok(self.inner.pickle().encrypt(key))
    }

    /// Encrypt plaintext. Returns a JS object:
    /// `{ messageType: number, ciphertext: Uint8Array }`
    ///
    /// `messageType` is 0 for PreKey, 1 for Normal.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<JsValue, JsError> {
        let olm_msg: OlmMessage = self.inner.encrypt(plaintext);
        let (msg_type, ciphertext) = olm_msg.to_parts();

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"messageType".into(), &(msg_type as u32).into())
            .map_err(|_| JsError::new("Reflect::set messageType"))?;
        js_sys::Reflect::set(
            &obj,
            &"ciphertext".into(),
            &js_sys::Uint8Array::from(ciphertext.as_slice()).into(),
        )
        .map_err(|_| JsError::new("Reflect::set ciphertext"))?;

        Ok(obj.into())
    }

    /// Decrypt an Olm message.
    ///
    /// `message_type` — 0 for PreKey, 1 for Normal.
    /// `ciphertext` — raw ciphertext bytes.
    ///
    /// Returns the plaintext as `Uint8Array`.
    pub fn decrypt(&mut self, message_type: u8, ciphertext: &[u8]) -> Result<Vec<u8>, JsError> {
        let olm_msg = OlmMessage::from_parts(message_type as usize, ciphertext)
            .map_err(|e| JsError::new(&format!("bad olm message: {e}")))?;

        self.inner
            .decrypt(&olm_msg)
            .map_err(|e| JsError::new(&format!("decrypt failed: {e}")))
    }

    /// Return the globally unique session ID (base64).
    #[wasm_bindgen(js_name = "sessionId")]
    pub fn session_id(&self) -> String {
        self.inner.session_id()
    }
}
