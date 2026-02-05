//! Crypto Service - Thread-safe Matrix crypto operations via message passing.
//!
//! The MatrixCryptoManager uses matrix-sdk-crypto which has !Send futures. This service
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
//! └──────────────┘        │    MatrixCryptoManager              │
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
//! let envelope = handle.encrypt_simple(b"hello").await?;
//!
//! // Decrypt from any thread
//! let plaintext = handle.decrypt(&envelope).await?;
//!
//! // Check session exists
//! let has = handle.has_session("peer-identity").await?;
//! ```
//!
//! Rust guideline compliant 2025-01

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use super::matrix_crypto::{CryptoEnvelope, DeviceKeyBundle, MatrixCryptoManager};

/// Request types for the crypto service.
///
/// Each variant includes a oneshot channel for returning the result.
/// The `reply` field is not Debug-printable, so we use custom Debug impl.
pub enum CryptoRequest {
    /// Encrypt plaintext for a specific peer (requires Curve25519 identity key).
    Encrypt {
        /// The plaintext bytes to encrypt.
        plaintext: Vec<u8>,
        /// The peer's Curve25519 identity key (base64).
        peer_curve25519_key: String,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<CryptoEnvelope>>,
    },

    /// Simple encrypt (wraps data without encryption - for QR flow).
    EncryptSimple {
        /// The plaintext bytes to encrypt.
        plaintext: Vec<u8>,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<CryptoEnvelope>>,
    },

    /// Decrypt an envelope from a peer.
    Decrypt {
        /// The encrypted envelope.
        envelope: CryptoEnvelope,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<Vec<u8>>>,
    },

    /// Check if we have a session with a peer.
    HasSession {
        /// The peer's Curve25519 identity key (base64).
        peer_curve25519_key: String,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<bool>>,
    },

    /// Get the device key bundle for QR code display.
    GetDeviceKeyBundle {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<DeviceKeyBundle>>,
    },

    /// Get our Curve25519 identity key (base64).
    GetIdentityKey {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<String>>,
    },

    /// Get our Ed25519 signing key (base64).
    GetSigningKey {
        /// Channel to send the result.
        reply: oneshot::Sender<Result<String>>,
    },

    /// Get the exported one-time key (key_id, curve25519_base64).
    ExportedOneTimeKey {
        /// Channel to send the result.
        reply: oneshot::Sender<Option<(String, String)>>,
    },

    /// Encrypt for room (Megolm group encryption).
    EncryptRoomEvent {
        /// The plaintext bytes to encrypt.
        plaintext: Vec<u8>,
        /// Channel to send the result.
        reply: oneshot::Sender<Result<CryptoEnvelope>>,
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
            Self::Encrypt {
                peer_curve25519_key,
                ..
            } => {
                let truncated: &str = &peer_curve25519_key[..peer_curve25519_key.len().min(16)];
                f.debug_struct("Encrypt")
                    .field("peer_curve25519_key", &truncated)
                    .finish_non_exhaustive()
            }
            Self::EncryptSimple { .. } => write!(f, "EncryptSimple"),
            Self::Decrypt { envelope, .. } => f
                .debug_struct("Decrypt")
                .field("message_type", &envelope.message_type)
                .finish_non_exhaustive(),
            Self::HasSession {
                peer_curve25519_key,
                ..
            } => {
                let truncated: &str = &peer_curve25519_key[..peer_curve25519_key.len().min(16)];
                f.debug_struct("HasSession")
                    .field("peer_curve25519_key", &truncated)
                    .finish_non_exhaustive()
            }
            Self::GetDeviceKeyBundle { .. } => write!(f, "GetDeviceKeyBundle"),
            Self::GetIdentityKey { .. } => write!(f, "GetIdentityKey"),
            Self::GetSigningKey { .. } => write!(f, "GetSigningKey"),
            Self::ExportedOneTimeKey { .. } => write!(f, "ExportedOneTimeKey"),
            Self::EncryptRoomEvent { .. } => write!(f, "EncryptRoomEvent"),
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
    pub async fn encrypt(
        &self,
        plaintext: &[u8],
        peer_curve25519_key: &str,
    ) -> Result<CryptoEnvelope> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::Encrypt {
                plaintext: plaintext.to_vec(),
                peer_curve25519_key: peer_curve25519_key.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Simple encrypt without specifying peer.
    pub async fn encrypt_simple(&self, plaintext: &[u8]) -> Result<CryptoEnvelope> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::EncryptSimple {
                plaintext: plaintext.to_vec(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Decrypt an envelope from a peer.
    pub async fn decrypt(&self, envelope: &CryptoEnvelope) -> Result<Vec<u8>> {
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
    pub async fn has_session(&self, peer_curve25519_key: &str) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::HasSession {
                peer_curve25519_key: peer_curve25519_key.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get the device key bundle for QR code display.
    pub async fn get_device_key_bundle(&self) -> Result<DeviceKeyBundle> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GetDeviceKeyBundle { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get our Curve25519 identity key (base64).
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

    /// Get our Ed25519 signing key (base64).
    pub async fn signing_key(&self) -> Result<String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::GetSigningKey { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service shut down"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Crypto service dropped response"))?
    }

    /// Get the exported one-time key (key_id, curve25519_base64).
    pub async fn exported_one_time_key(&self) -> Option<(String, String)> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(CryptoRequest::ExportedOneTimeKey { reply: reply_tx })
            .await
            .is_err()
        {
            return None;
        }

        reply_rx.await.ok().flatten()
    }

    /// Encrypt for room (Megolm group encryption).
    pub async fn encrypt_room_event(&self, plaintext: &[u8]) -> Result<CryptoEnvelope> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(CryptoRequest::EncryptRoomEvent {
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
    ///
    /// Hybrid persist strategy: session state is persisted when any of:
    /// 1. Operation count reaches 200 (handles high-throughput bursts)
    /// 2. Timer fires every 30s with dirty state (catches low-throughput drift)
    /// 3. Shutdown (ensures clean exit)
    ///
    /// On crash, worst case loses 200 ops of ratchet state. Session
    /// re-establishment is cheap (~1 RTT for new QR scan).
    async fn run_service(hub_id: &str, mut rx: mpsc::Receiver<CryptoRequest>) -> Result<()> {
        // Load or create the Matrix crypto manager
        let manager = MatrixCryptoManager::load_or_create(hub_id).await?;
        let mut dirty = false;
        let mut ops_since_persist: u32 = 0;
        let mut persist_interval = tokio::time::interval(std::time::Duration::from_secs(30));
        persist_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        log::debug!("Crypto service ready, waiting for requests");

        loop {
            tokio::select! {
                request = rx.recv() => {
                    let Some(request) = request else { break; };
                    match request {
                        CryptoRequest::Encrypt {
                            plaintext,
                            peer_curve25519_key,
                            reply,
                        } => {
                            let result = manager.encrypt(&plaintext, &peer_curve25519_key).await;
                            if result.is_ok() {
                                dirty = true;
                                ops_since_persist += 1;
                            }
                            let _ = reply.send(result);
                        }

                        CryptoRequest::EncryptSimple { plaintext, reply } => {
                            let result = manager.encrypt_simple(&plaintext).await;
                            if result.is_ok() {
                                dirty = true;
                                ops_since_persist += 1;
                            }
                            let _ = reply.send(result);
                        }

                        CryptoRequest::Decrypt { envelope, reply } => {
                            let result = manager.decrypt(&envelope).await;
                            if result.is_ok() {
                                dirty = true;
                                ops_since_persist += 1;
                            }
                            let _ = reply.send(result);
                        }

                        CryptoRequest::HasSession {
                            peer_curve25519_key,
                            reply,
                        } => {
                            let result = manager.has_session(&peer_curve25519_key).await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::GetDeviceKeyBundle { reply } => {
                            let result = manager.build_device_key_bundle().await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::GetIdentityKey { reply } => {
                            let result = manager.identity_key().await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::GetSigningKey { reply } => {
                            let result = manager.signing_key().await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::ExportedOneTimeKey { reply } => {
                            let result = manager.exported_one_time_key().await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::EncryptRoomEvent { plaintext, reply } => {
                            let result = manager.encrypt_room_event(&plaintext).await;
                            let _ = reply.send(result);
                        }

                        CryptoRequest::Persist { reply } => {
                            let result = manager.persist().await;
                            if result.is_ok() {
                                dirty = false;
                                ops_since_persist = 0;
                            }
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

                    // Persist immediately if operation count threshold reached
                    if ops_since_persist >= 200 {
                        if let Err(e) = manager.persist().await {
                            log::warn!("Op-count persist failed: {e}");
                        } else {
                            dirty = false;
                            ops_since_persist = 0;
                            log::debug!("Op-count persist completed (200 ops)");
                        }
                    }
                }
                _ = persist_interval.tick() => {
                    if dirty {
                        if let Err(e) = manager.persist().await {
                            log::warn!("Periodic persist failed: {e}");
                        } else {
                            dirty = false;
                            ops_since_persist = 0;
                            log::debug!("Periodic persist completed");
                        }
                    }
                }
            }
        }

        // Final persist on exit - catches channel closure (Hub dropped) without
        // explicit Shutdown request. Harmless double-persist if Shutdown already ran.
        if dirty {
            if let Err(e) = manager.persist().await {
                log::warn!("Final persist on service exit failed: {e}");
            } else {
                log::info!("Final persist completed on service exit");
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
    async fn test_crypto_service_device_key_bundle() {
        let handle = CryptoService::start("test-crypto-bundle").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let bundle = handle.get_device_key_bundle().await.unwrap();
        assert_eq!(bundle.version, 5); // Matrix version
        assert!(!bundle.curve25519_key.is_empty());
        assert!(!bundle.ed25519_key.is_empty());

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_session_check() {
        let handle = CryptoService::start("test-crypto-session").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // No session should exist for unknown peer
        let has_session = handle.has_session("unknown-peer-key").await.unwrap();
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
        let id1 = handle.identity_key().await.unwrap();
        let id2 = handle2.identity_key().await.unwrap();
        assert_eq!(id1, id2);

        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_crypto_service_concurrent_requests() {
        let handle = CryptoService::start("test-crypto-concurrent").unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // First request a bundle to trigger key generation
        let _bundle = handle.get_device_key_bundle().await.unwrap();

        // Fire off multiple concurrent requests
        let h1 = handle.clone();
        let h2 = handle.clone();
        let h3 = handle.clone();

        let (r1, r2, r3) = tokio::join!(
            h1.identity_key(),
            h2.signing_key(),
            h3.exported_one_time_key()
        );

        assert!(r1.is_ok());
        assert!(r2.is_ok());
        // One-time key may or may not be available depending on generation
        let _ = r3;

        handle.shutdown().await.unwrap();
    }

    /// Test that session state is shared across handle clones.
    ///
    /// This simulates session reuse between WebRTC and agent channels:
    /// - WebRTC uses handle1 to check/create sessions
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
        let id1 = handle1.identity_key().await.unwrap();
        let id2 = handle2.identity_key().await.unwrap();
        assert_eq!(id1, id2, "Both handles should return same identity key");

        handle1.shutdown().await.unwrap();
    }
}
