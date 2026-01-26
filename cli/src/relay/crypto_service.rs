//! Crypto Service - Thread-safe Signal Protocol operations via message passing.
//!
//! The SignalProtocolManager uses libsignal which has !Send futures. This service
//! runs the manager in a dedicated thread with a LocalSet, exposing a Send-safe
//! handle that other threads can use to request crypto operations.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────┐        ┌─────────────────────────────────────┐
//! │ Terminal     │        │       CRYPTO SERVICE THREAD         │
//! │ Relay Thread │──req──▶│                                     │
//! │              │◀──res──│  LocalSet {                         │
//! └──────────────┘        │    SignalProtocolManager            │
//!                         │    process_requests() loop          │
//! ┌──────────────┐        │  }                                  │
//! │ Preview      │        │                                     │
//! │ Relay Thread │──req──▶│                                     │
//! │              │◀──res──│                                     │
//! └──────────────┘        └─────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! // Start the service (spawns thread)
//! let handle = CryptoService::start("hub-id").await?;
//!
//! // Encrypt from any thread
//! let envelope = handle.encrypt(b"hello", "peer-identity").await?;
//!
//! // Decrypt from any thread
//! let plaintext = handle.decrypt(&envelope).await?;
//!
//! // Check session exists
//! let has = handle.has_session("peer-identity").await?;
//! ```

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use super::signal::{PreKeyBundleData, SignalEnvelope, SignalProtocolManager};

/// Request types for the crypto service.
///
/// Each variant includes a oneshot channel for returning the result.
/// The `reply` field is not Debug-printable, so we use custom Debug impl.
pub enum CryptoRequest {
    /// Encrypt plaintext for a specific peer.
    Encrypt {
        /// The plaintext bytes to encrypt.
        plaintext: Vec<u8>,
        /// The peer's identity key (base64).
        peer_identity: String,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<SignalEnvelope>>,
    },

    /// Decrypt an envelope from a peer.
    Decrypt {
        /// The encrypted envelope.
        envelope: SignalEnvelope,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },

    /// Check if we have a session with a peer.
    HasSession {
        /// The peer's identity key (base64).
        peer_identity: String,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<bool>>,
    },

    /// Get the PreKey bundle for QR code display.
    GetPreKeyBundle {
        /// Preferred PreKey ID to use.
        preferred_prekey_id: u32,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<PreKeyBundleData>>,
    },

    /// Get our identity key (base64).
    GetIdentityKey {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<String>>,
    },

    /// Get our registration ID.
    GetRegistrationId {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<u32>>,
    },

    /// Get next available PreKey ID.
    NextPreKeyId {
        /// Channel to send the result.
        reply: oneshot::Sender<Option<u32>>,
    },

    /// Create SenderKey distribution message for group broadcasts.
    CreateSenderKeyDistribution {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },

    /// Encrypt for all group members (broadcast).
    GroupEncrypt {
        /// The plaintext bytes to encrypt.
        plaintext: Vec<u8>,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<SignalEnvelope>>,
    },

    /// Persist state to disk.
    Persist {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<()>>,
    },

    /// Shutdown the service.
    Shutdown,
}

impl std::fmt::Debug for CryptoRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encrypt { peer_identity, .. } => {
                let truncated: &str = &peer_identity[..peer_identity.len().min(16)];
                f.debug_struct("Encrypt")
                    .field("peer_identity", &truncated)
                    .finish_non_exhaustive()
            }
            Self::Decrypt { envelope, .. } => f
                .debug_struct("Decrypt")
                .field("message_type", &envelope.message_type)
                .finish_non_exhaustive(),
            Self::HasSession { peer_identity, .. } => {
                let truncated: &str = &peer_identity[..peer_identity.len().min(16)];
                f.debug_struct("HasSession")
                    .field("peer_identity", &truncated)
                    .finish_non_exhaustive()
            }
            Self::GetPreKeyBundle {
                preferred_prekey_id,
                ..
            } => f
                .debug_struct("GetPreKeyBundle")
                .field("preferred_prekey_id", preferred_prekey_id)
                .finish_non_exhaustive(),
            Self::GetIdentityKey { .. } => write!(f, "GetIdentityKey"),
            Self::GetRegistrationId { .. } => write!(f, "GetRegistrationId"),
            Self::NextPreKeyId { .. } => write!(f, "NextPreKeyId"),
            Self::CreateSenderKeyDistribution { .. } => write!(f, "CreateSenderKeyDistribution"),
            Self::GroupEncrypt { .. } => write!(f, "GroupEncrypt"),
            Self::Persist { .. } => write!(f, "Persist"),
            Self::Shutdown => write!(f, "Shutdown"),
        }
    }
}

/// Handle for sending requests to the crypto service.
///
/// This is Send + Sync and can be cloned and shared across threads.
#[derive(Clone, Debug)]
pub struct CryptoServiceHandle {
    tx: mpsc::Sender<CryptoRequest>,
}

impl CryptoServiceHandle {
    /// Encrypt plaintext for a specific peer.
    pub async fn encrypt(&self, plaintext: &[u8], peer_identity: &str) -> Result<SignalEnvelope> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::Encrypt {
                plaintext: plaintext.to_vec(),
                peer_identity: peer_identity.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Decrypt an envelope from a peer.
    pub async fn decrypt(&self, envelope: &SignalEnvelope) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::Decrypt {
                envelope: envelope.clone(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Check if we have a session with a peer.
    pub async fn has_session(&self, peer_identity: &str) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::HasSession {
                peer_identity: peer_identity.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get the PreKey bundle for QR code display.
    pub async fn get_prekey_bundle(&self, preferred_prekey_id: u32) -> Result<PreKeyBundleData> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GetPreKeyBundle {
                preferred_prekey_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get our identity key (base64).
    pub async fn identity_key(&self) -> Result<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GetIdentityKey { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get our registration ID.
    pub async fn registration_id(&self) -> Result<u32> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GetRegistrationId { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get next available PreKey ID.
    pub async fn next_prekey_id(&self) -> Option<u32> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(CryptoRequest::NextPreKeyId { reply: reply_tx })
            .await
            .is_err()
        {
            return None;
        }

        reply_rx.await.ok().flatten()
    }

    /// Create SenderKey distribution message for group broadcasts.
    pub async fn create_sender_key_distribution(&self) -> Result<Vec<u8>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::CreateSenderKeyDistribution { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Encrypt for all group members (broadcast).
    pub async fn group_encrypt(&self, plaintext: &[u8]) -> Result<SignalEnvelope> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GroupEncrypt {
                plaintext: plaintext.to_vec(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Persist state to disk.
    pub async fn persist(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::Persist { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Shutdown the service.
    pub async fn shutdown(&self) -> Result<()> {
        self.tx
            .send(CryptoRequest::Shutdown)
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service already shut down"))
    }

    /// Create a mock handle for testing.
    ///
    /// Creates a handle with a closed channel - operations will fail gracefully.
    /// Suitable for tests that don't need actual crypto operations.
    #[cfg(test)]
    #[must_use]
    pub fn mock() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self { tx }
    }
}

/// The crypto service that runs in its own thread.
#[derive(Debug)]
pub struct CryptoService;

impl CryptoService {
    /// Start the crypto service in a dedicated thread.
    ///
    /// Returns a handle that can be used from any thread to request crypto operations.
    pub fn start(hub_id: &str) -> Result<CryptoServiceHandle> {
        let hub_id = hub_id.to_string();
        let (tx, rx) = mpsc::channel::<CryptoRequest>(256);

        // Spawn a dedicated thread for the crypto service
        let thread_hub_id = hub_id.clone();
        std::thread::Builder::new()
            .name("crypto-service".to_string())
            .spawn(move || {
                // Create a single-threaded runtime for !Send futures
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build crypto service runtime");

                let local = tokio::task::LocalSet::new();

                local.block_on(&rt, async move {
                    if let Err(e) = Self::run_service(&thread_hub_id, rx).await {
                        log::error!("Crypto service error: {e}");
                    }
                });
            })
            .context("Failed to spawn crypto service thread")?;

        log::info!(
            "Started crypto service for hub {}",
            &hub_id[..hub_id.len().min(8)]
        );

        Ok(CryptoServiceHandle { tx })
    }

    /// Main service loop - processes requests until shutdown.
    async fn run_service(hub_id: &str, mut rx: mpsc::Receiver<CryptoRequest>) -> Result<()> {
        // Load or create the Signal protocol manager
        let mut manager = SignalProtocolManager::load_or_create(hub_id).await?;

        log::debug!("Crypto service ready, waiting for requests");

        while let Some(request) = rx.recv().await {
            match request {
                CryptoRequest::Encrypt {
                    plaintext,
                    peer_identity,
                    reply,
                } => {
                    let result = manager.encrypt(&plaintext, &peer_identity).await;
                    let _ = reply.send(result);
                }

                CryptoRequest::Decrypt { envelope, reply } => {
                    let result = manager.decrypt(&envelope).await;
                    let _ = reply.send(result);
                }

                CryptoRequest::HasSession {
                    peer_identity,
                    reply,
                } => {
                    let result = manager.has_session(&peer_identity).await;
                    let _ = reply.send(result);
                }

                CryptoRequest::GetPreKeyBundle {
                    preferred_prekey_id,
                    reply,
                } => {
                    let result = manager.build_prekey_bundle_data(preferred_prekey_id).await;
                    let _ = reply.send(result);
                }

                CryptoRequest::GetIdentityKey { reply } => {
                    let result = manager.identity_key().await;
                    let _ = reply.send(result);
                }

                CryptoRequest::GetRegistrationId { reply } => {
                    let result = manager.registration_id().await;
                    let _ = reply.send(result);
                }

                CryptoRequest::NextPreKeyId { reply } => {
                    let result = manager.next_prekey_id().await;
                    let _ = reply.send(result);
                }

                CryptoRequest::CreateSenderKeyDistribution { reply } => {
                    let result = manager.create_sender_key_distribution().await;
                    let _ = reply.send(result);
                }

                CryptoRequest::GroupEncrypt { plaintext, reply } => {
                    let result = manager.group_encrypt(&plaintext).await;
                    let _ = reply.send(result);
                }

                CryptoRequest::Persist { reply } => {
                    let result = manager.persist().await;
                    let _ = reply.send(result);
                }

                CryptoRequest::Shutdown => {
                    log::info!("Crypto service shutting down");
                    // Persist before shutdown
                    if let Err(e) = manager.persist().await {
                        log::warn!("Failed to persist on shutdown: {e}");
                    }
                    break;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_crypto_service_starts() {
        let handle = CryptoService::start("test-crypto-service").unwrap();

        // Give the thread time to start
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Should be able to get identity key
        let identity = handle.identity_key().await.unwrap();
        assert!(!identity.is_empty());

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_prekey_bundle() {
        let handle = CryptoService::start("test-crypto-bundle").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let bundle = handle.get_prekey_bundle(1).await.unwrap();
        assert_eq!(bundle.version, 4);
        assert!(!bundle.identity_key.is_empty());
        assert!(!bundle.signed_prekey.is_empty());

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_session_check() {
        let handle = CryptoService::start("test-crypto-session").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // No session should exist for unknown peer
        let has_session = handle.has_session("unknown-peer").await.unwrap();
        assert!(!has_session);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_handle_is_clone() {
        let handle = CryptoService::start("test-crypto-clone").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Clone the handle
        let handle2 = handle.clone();

        // Both should work
        let id1 = handle.registration_id().await.unwrap();
        let id2 = handle2.registration_id().await.unwrap();
        assert_eq!(id1, id2);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_concurrent_requests() {
        let handle = CryptoService::start("test-crypto-concurrent").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // First request a bundle to trigger lazy key generation
        let _bundle = handle.get_prekey_bundle(1).await.unwrap();

        // Fire off multiple concurrent requests
        let h1 = handle.clone();
        let h2 = handle.clone();
        let h3 = handle.clone();

        let (r1, r2, r3) =
            tokio::join!(h1.identity_key(), h2.registration_id(), h3.next_prekey_id());

        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(r3.is_some(), "PreKeys should exist after bundle request");

        handle.shutdown().await.unwrap();
    }

    /// Test that session state is shared across handle clones.
    ///
    /// This simulates session reuse between HubRelay and agent channels:
    /// - HubRelay uses handle1 to check/create sessions
    /// - Agent channels use handle clones and should see same sessions
    #[tokio::test]
    async fn test_crypto_service_session_shared_across_clones() {
        let handle1 = CryptoService::start("test-session-shared").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let handle2 = handle1.clone();

        // Check session via handle1 - should not exist
        let has_before = handle1.has_session("peer-abc").await.unwrap();
        assert!(!has_before, "Session should not exist initially");

        // Verify handle2 sees the same state
        let has_before_h2 = handle2.has_session("peer-abc").await.unwrap();
        assert!(!has_before_h2, "Clone should also see no session");

        // After a session is established (via decrypt from a peer),
        // both handles should see it. We can't easily test decrypt without
        // a real peer, but we can verify both handles hit the same manager.
        let id1 = handle1.registration_id().await.unwrap();
        let id2 = handle2.registration_id().await.unwrap();
        assert_eq!(id1, id2, "Both handles should return same registration ID");

        handle1.shutdown().await.unwrap();
    }
}
