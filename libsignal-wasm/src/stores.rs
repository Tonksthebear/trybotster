//! Browser-side Signal Protocol stores.
//!
//! In-memory implementations of the 6 Signal Protocol store traits,
//! with serialization support for IndexedDB persistence.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use libsignal_protocol::{
    // Store traits
    IdentityKeyStore, SessionStore, PreKeyStore, SignedPreKeyStore, KyberPreKeyStore, SenderKeyStore,
    GenericSignedPreKey,
    // Types
    IdentityKey, IdentityKeyPair, IdentityChange, ProtocolAddress, DeviceId, Direction, PublicKey,
    SessionRecord, PreKeyRecord, SignedPreKeyRecord, KyberPreKeyRecord, SenderKeyRecord,
    PreKeyId, SignedPreKeyId, KyberPreKeyId,
};
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Browser device ID (CLI = 1, browsers start at 2).
const BROWSER_DEVICE_ID: u32 = 2;

/// Result type for store operations.
type StoreResult<T> = std::result::Result<T, libsignal_protocol::SignalProtocolError>;

/// Serializable store state for pickle/unpickle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickledStore {
    /// Identity key pair (private + public).
    identity_key_pair: String,
    /// Registration ID.
    registration_id: u32,
    /// Known peer identities: address -> identity key.
    identities: HashMap<String, String>,
    /// Sessions: address -> session record.
    sessions: HashMap<String, String>,
    /// PreKeys: id -> record (browser usually doesn't need many).
    prekeys: HashMap<u32, String>,
    /// Signed PreKeys: id -> record.
    signed_prekeys: HashMap<u32, String>,
    /// Kyber PreKeys: id -> record.
    kyber_prekeys: HashMap<u32, String>,
    /// Sender keys: (sender, distribution_id) -> record.
    sender_keys: HashMap<String, String>,
    /// CLI address we're connected to.
    cli_address_name: String,
    cli_address_device_id: u8,
    /// Hub ID.
    hub_id: String,
}

/// In-memory Signal Protocol store for browser.
///
/// Uses Rc<RefCell<...>> since WASM is single-threaded.
#[derive(Clone)]
pub struct BrowserSignalStore {
    identity_key_pair: Rc<RefCell<IdentityKeyPair>>,
    registration_id: u32,
    identities: Rc<RefCell<HashMap<String, IdentityKey>>>,
    sessions: Rc<RefCell<HashMap<String, SessionRecord>>>,
    prekeys: Rc<RefCell<HashMap<u32, PreKeyRecord>>>,
    signed_prekeys: Rc<RefCell<HashMap<u32, SignedPreKeyRecord>>>,
    kyber_prekeys: Rc<RefCell<HashMap<u32, KyberPreKeyRecord>>>,
    sender_keys: Rc<RefCell<HashMap<String, SenderKeyRecord>>>,
}

impl BrowserSignalStore {
    /// Create a new store with fresh identity.
    pub async fn new() -> anyhow::Result<Self> {
        // Generate fresh identity for this browser session
        let identity_key_pair = IdentityKeyPair::generate(&mut rand::rngs::StdRng::from_os_rng());
        let registration_id = rand::random::<u32>() & 0x3FFF; // 14-bit ID

        Ok(Self {
            identity_key_pair: Rc::new(RefCell::new(identity_key_pair)),
            registration_id,
            identities: Rc::new(RefCell::new(HashMap::new())),
            sessions: Rc::new(RefCell::new(HashMap::new())),
            prekeys: Rc::new(RefCell::new(HashMap::new())),
            signed_prekeys: Rc::new(RefCell::new(HashMap::new())),
            kyber_prekeys: Rc::new(RefCell::new(HashMap::new())),
            sender_keys: Rc::new(RefCell::new(HashMap::new())),
        })
    }

    /// Pickle the store for IndexedDB storage.
    pub fn pickle(&self, cli_address: &ProtocolAddress, hub_id: &str) -> anyhow::Result<String> {
        let identity_key_pair = self.identity_key_pair.borrow();
        let identities = self.identities.borrow();
        let sessions = self.sessions.borrow();
        let prekeys = self.prekeys.borrow();
        let signed_prekeys = self.signed_prekeys.borrow();
        let kyber_prekeys = self.kyber_prekeys.borrow();
        let sender_keys = self.sender_keys.borrow();

        let pickled = PickledStore {
            identity_key_pair: BASE64.encode(identity_key_pair.serialize()),
            registration_id: self.registration_id,
            identities: identities
                .iter()
                .map(|(k, v)| (k.clone(), BASE64.encode(v.serialize())))
                .collect(),
            sessions: sessions
                .iter()
                .map(|(k, v)| (k.clone(), BASE64.encode(v.serialize().expect("serialize session"))))
                .collect(),
            prekeys: prekeys
                .iter()
                .map(|(k, v)| (*k, BASE64.encode(v.serialize().expect("serialize prekey"))))
                .collect(),
            signed_prekeys: signed_prekeys
                .iter()
                .map(|(k, v)| (*k, BASE64.encode(v.serialize().expect("serialize signed_prekey"))))
                .collect(),
            kyber_prekeys: kyber_prekeys
                .iter()
                .map(|(k, v)| (*k, BASE64.encode(v.serialize().expect("serialize kyber_prekey"))))
                .collect(),
            sender_keys: sender_keys
                .iter()
                .map(|(k, v)| (k.clone(), BASE64.encode(v.serialize().expect("serialize sender_key"))))
                .collect(),
            cli_address_name: cli_address.name().to_string(),
            cli_address_device_id: u32::from(cli_address.device_id()) as u8,
            hub_id: hub_id.to_string(),
        };

        serde_json::to_string(&pickled).map_err(|e| anyhow::anyhow!("Pickle serialize: {e}"))
    }

    /// Restore store from pickled string.
    pub fn from_pickle(pickle: &str) -> anyhow::Result<(Self, ProtocolAddress, String)> {
        let pickled: PickledStore = serde_json::from_str(pickle)
            .map_err(|e| anyhow::anyhow!("Pickle deserialize: {e}"))?;

        let identity_key_pair_bytes = BASE64.decode(&pickled.identity_key_pair)
            .map_err(|e| anyhow::anyhow!("identity_key_pair decode: {e}"))?;
        let identity_key_pair = IdentityKeyPair::try_from(identity_key_pair_bytes.as_slice())
            .map_err(|e| anyhow::anyhow!("identity_key_pair parse: {e}"))?;

        let mut identities = HashMap::new();
        for (k, v) in pickled.identities {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("identity decode: {e}"))?;
            let key = IdentityKey::decode(&bytes).map_err(|e| anyhow::anyhow!("identity parse: {e}"))?;
            identities.insert(k, key);
        }

        let mut sessions = HashMap::new();
        for (k, v) in pickled.sessions {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("session decode: {e}"))?;
            let record = SessionRecord::deserialize(&bytes).map_err(|e| anyhow::anyhow!("session parse: {e}"))?;
            sessions.insert(k, record);
        }

        let mut prekeys = HashMap::new();
        for (k, v) in pickled.prekeys {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("prekey decode: {e}"))?;
            let record = PreKeyRecord::deserialize(&bytes).map_err(|e| anyhow::anyhow!("prekey parse: {e}"))?;
            prekeys.insert(k, record);
        }

        let mut signed_prekeys = HashMap::new();
        for (k, v) in pickled.signed_prekeys {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("signed_prekey decode: {e}"))?;
            let record = SignedPreKeyRecord::deserialize(&bytes).map_err(|e| anyhow::anyhow!("signed_prekey parse: {e}"))?;
            signed_prekeys.insert(k, record);
        }

        let mut kyber_prekeys = HashMap::new();
        for (k, v) in pickled.kyber_prekeys {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("kyber_prekey decode: {e}"))?;
            let record = KyberPreKeyRecord::deserialize(&bytes).map_err(|e| anyhow::anyhow!("kyber_prekey parse: {e}"))?;
            kyber_prekeys.insert(k, record);
        }

        let mut sender_keys = HashMap::new();
        for (k, v) in pickled.sender_keys {
            let bytes = BASE64.decode(&v).map_err(|e| anyhow::anyhow!("sender_key decode: {e}"))?;
            let record = SenderKeyRecord::deserialize(&bytes).map_err(|e| anyhow::anyhow!("sender_key parse: {e}"))?;
            sender_keys.insert(k, record);
        }

        let cli_address = ProtocolAddress::new(
            pickled.cli_address_name,
            DeviceId::new(pickled.cli_address_device_id).expect("valid device ID"),
        );

        let store = Self {
            identity_key_pair: Rc::new(RefCell::new(identity_key_pair)),
            registration_id: pickled.registration_id,
            identities: Rc::new(RefCell::new(identities)),
            sessions: Rc::new(RefCell::new(sessions)),
            prekeys: Rc::new(RefCell::new(prekeys)),
            signed_prekeys: Rc::new(RefCell::new(signed_prekeys)),
            kyber_prekeys: Rc::new(RefCell::new(kyber_prekeys)),
            sender_keys: Rc::new(RefCell::new(sender_keys)),
        };

        Ok((store, cli_address, pickled.hub_id))
    }

    /// Get identity key pair (for encryption envelope).
    pub async fn get_identity_key_pair(&self) -> StoreResult<IdentityKeyPair> {
        Ok(self.identity_key_pair.borrow().clone())
    }

    /// Get registration ID (for encryption envelope).
    pub async fn get_local_registration_id(&self) -> StoreResult<u32> {
        Ok(self.registration_id)
    }

    /// Make key for session/identity maps.
    fn address_key(address: &ProtocolAddress) -> String {
        format!("{}:{}", address.name(), u32::from(address.device_id()))
    }

    /// Make key for sender key map.
    fn sender_key_key(sender: &ProtocolAddress, distribution_id: uuid::Uuid) -> String {
        format!("{}:{}:{}", sender.name(), u32::from(sender.device_id()), distribution_id)
    }
}

// ============================================================================
// IdentityKeyStore
// ============================================================================

#[async_trait(?Send)]
impl IdentityKeyStore for BrowserSignalStore {
    async fn get_identity_key_pair(&self) -> StoreResult<IdentityKeyPair> {
        Ok(self.identity_key_pair.borrow().clone())
    }

    async fn get_local_registration_id(&self) -> StoreResult<u32> {
        Ok(self.registration_id)
    }

    async fn save_identity(
        &mut self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
    ) -> StoreResult<IdentityChange> {
        let key = Self::address_key(address);
        let mut identities = self.identities.borrow_mut();
        let existing = identities.get(&key).cloned();
        identities.insert(key, identity.clone());
        let changed = existing.as_ref() != Some(identity) && existing.is_some();
        Ok(IdentityChange::from_changed(changed))
    }

    async fn is_trusted_identity(
        &self,
        address: &ProtocolAddress,
        identity: &IdentityKey,
        _direction: Direction,
    ) -> StoreResult<bool> {
        let key = Self::address_key(address);
        let identities = self.identities.borrow();
        match identities.get(&key) {
            Some(existing) => Ok(existing == identity),
            None => Ok(true), // First contact - trust on first use
        }
    }

    async fn get_identity(&self, address: &ProtocolAddress) -> StoreResult<Option<IdentityKey>> {
        let key = Self::address_key(address);
        Ok(self.identities.borrow().get(&key).cloned())
    }
}

// ============================================================================
// SessionStore
// ============================================================================

#[async_trait(?Send)]
impl SessionStore for BrowserSignalStore {
    async fn load_session(&self, address: &ProtocolAddress) -> StoreResult<Option<SessionRecord>> {
        let key = Self::address_key(address);
        Ok(self.sessions.borrow().get(&key).cloned())
    }

    async fn store_session(
        &mut self,
        address: &ProtocolAddress,
        record: &SessionRecord,
    ) -> StoreResult<()> {
        let key = Self::address_key(address);
        self.sessions.borrow_mut().insert(key, record.clone());
        Ok(())
    }
}

// ============================================================================
// PreKeyStore
// ============================================================================

#[async_trait(?Send)]
impl PreKeyStore for BrowserSignalStore {
    async fn get_pre_key(&self, prekey_id: PreKeyId) -> StoreResult<PreKeyRecord> {
        let id: u32 = prekey_id.into();
        self.prekeys
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(libsignal_protocol::SignalProtocolError::InvalidPreKeyId)
    }

    async fn save_pre_key(&mut self, prekey_id: PreKeyId, record: &PreKeyRecord) -> StoreResult<()> {
        let id: u32 = prekey_id.into();
        self.prekeys.borrow_mut().insert(id, record.clone());
        Ok(())
    }

    async fn remove_pre_key(&mut self, prekey_id: PreKeyId) -> StoreResult<()> {
        let id: u32 = prekey_id.into();
        self.prekeys.borrow_mut().remove(&id);
        Ok(())
    }
}

// ============================================================================
// SignedPreKeyStore
// ============================================================================

#[async_trait(?Send)]
impl SignedPreKeyStore for BrowserSignalStore {
    async fn get_signed_pre_key(&self, signed_prekey_id: SignedPreKeyId) -> StoreResult<SignedPreKeyRecord> {
        let id: u32 = signed_prekey_id.into();
        self.signed_prekeys
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(libsignal_protocol::SignalProtocolError::InvalidSignedPreKeyId)
    }

    async fn save_signed_pre_key(
        &mut self,
        signed_prekey_id: SignedPreKeyId,
        record: &SignedPreKeyRecord,
    ) -> StoreResult<()> {
        let id: u32 = signed_prekey_id.into();
        self.signed_prekeys.borrow_mut().insert(id, record.clone());
        Ok(())
    }
}

// ============================================================================
// KyberPreKeyStore
// ============================================================================

#[async_trait(?Send)]
impl KyberPreKeyStore for BrowserSignalStore {
    async fn get_kyber_pre_key(&self, kyber_prekey_id: KyberPreKeyId) -> StoreResult<KyberPreKeyRecord> {
        let id: u32 = kyber_prekey_id.into();
        self.kyber_prekeys
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(libsignal_protocol::SignalProtocolError::InvalidKyberPreKeyId)
    }

    async fn save_kyber_pre_key(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        record: &KyberPreKeyRecord,
    ) -> StoreResult<()> {
        let id: u32 = kyber_prekey_id.into();
        self.kyber_prekeys.borrow_mut().insert(id, record.clone());
        Ok(())
    }

    async fn mark_kyber_pre_key_used(
        &mut self,
        kyber_prekey_id: KyberPreKeyId,
        _ec_prekey_id: SignedPreKeyId,
        _base_key: &PublicKey,
    ) -> StoreResult<()> {
        // For browser, we can just remove after use (one-time keys)
        let id: u32 = kyber_prekey_id.into();
        self.kyber_prekeys.borrow_mut().remove(&id);
        Ok(())
    }
}

// ============================================================================
// SenderKeyStore
// ============================================================================

#[async_trait(?Send)]
impl SenderKeyStore for BrowserSignalStore {
    async fn store_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: uuid::Uuid,
        record: &SenderKeyRecord,
    ) -> StoreResult<()> {
        let key = Self::sender_key_key(sender, distribution_id);
        self.sender_keys.borrow_mut().insert(key, record.clone());
        Ok(())
    }

    async fn load_sender_key(
        &mut self,
        sender: &ProtocolAddress,
        distribution_id: uuid::Uuid,
    ) -> StoreResult<Option<SenderKeyRecord>> {
        let key = Self::sender_key_key(sender, distribution_id);
        Ok(self.sender_keys.borrow().get(&key).cloned())
    }
}
