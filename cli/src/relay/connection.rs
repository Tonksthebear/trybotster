//! Hub relay for E2E encrypted hub-level communication with browsers.
//!
//! This module handles hub-level commands and responses between CLI and browsers:
//! - Agent list updates (broadcast to all browsers)
//! - Agent creation progress (broadcast)
//! - Browser commands (create agent, select agent, etc.)
//! - Browser handshake and connection management
//!
//! Terminal I/O (PTY output/input) is handled by agent-owned channels, not this relay.
//! See `cli/src/channel/action_cable.rs` for per-agent terminal channels.
//!
//! # Architecture
//!
//! Uses `ActionCableChannel` internally with reliability enabled. The channel handles:
//! - WebSocket connection and reconnection
//! - E2E encryption via CryptoServiceHandle
//! - Reliable delivery (seq numbers, ACKs, retransmit, reorder buffer)
//!
//! Rust guideline compliant 2025-01

use anyhow::Result;
use data_encoding::BASE32_NOPAD;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};

use super::crypto_service::CryptoServiceHandle;
use super::state::IdentifiedBrowserEvent;
use super::types::{BrowserCommand, BrowserEvent, BrowserResize, TerminalMessage};
use crate::channel::{ActionCableChannel, Channel, ChannelConfig, IncomingMessage, PeerId};

/// Output message for relay task.
///
/// Private to this module in production, but exposed for testing.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub(crate) enum OutputMessage {
    /// Broadcast to all connected browsers.
    Broadcast(String),
    /// Send to a specific browser by identity.
    Targeted {
        /// Target browser identity.
        identity: String,
        /// Data to send.
        data: String,
    },
    /// Request to regenerate the PreKeyBundle with a fresh PreKey.
    RegenerateBundle,
}

/// Handle for sending terminal output to the browser.
///
/// This is a simple channel sender that queues output for the relay task.
#[derive(Clone)]
pub struct HubSender {
    tx: mpsc::Sender<OutputMessage>,
    connected: Arc<RwLock<bool>>,
}

impl std::fmt::Debug for HubSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubSender").finish_non_exhaustive()
    }
}

impl HubSender {
    /// Send terminal output to all browsers (will be encrypted by relay task).
    pub async fn send(&self, output: &str) -> Result<()> {
        if !*self.connected.read().await {
            return Ok(()); // Silently drop if no browser connected
        }

        self.tx
            .send(OutputMessage::Broadcast(output.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to queue output: {}", e))
    }

    /// Send terminal output to a specific browser by identity.
    pub async fn send_to(&self, identity: &str, output: &str) -> Result<()> {
        if !*self.connected.read().await {
            return Ok(()); // Silently drop if no browser connected
        }

        self.tx
            .send(OutputMessage::Targeted {
                identity: identity.to_string(),
                data: output.to_string(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to queue targeted output: {}", e))
    }

    /// Check if browser is connected and ready for encrypted communication.
    pub async fn is_ready(&self) -> bool {
        *self.connected.read().await
    }

    /// Request regeneration of the PreKeyBundle with a fresh PreKey.
    pub async fn request_bundle_regeneration(&self) -> Result<()> {
        self.tx
            .send(OutputMessage::RegenerateBundle)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to request bundle regeneration: {}", e))
    }

}

/// Hub relay connection manager.
///
/// Uses `ActionCableChannel` with reliability enabled for all communication.
pub struct HubRelay {
    crypto_service: CryptoServiceHandle,
    hub_identifier: String,
    server_url: String,
    api_key: String,
}

impl std::fmt::Debug for HubRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubRelay")
            .field("hub_identifier", &self.hub_identifier)
            .field("server_url", &self.server_url)
            .finish_non_exhaustive()
    }
}

impl HubRelay {
    /// Create a new hub relay.
    pub fn new(
        crypto_service: CryptoServiceHandle,
        hub_identifier: String,
        server_url: String,
        api_key: String,
    ) -> Self {
        Self {
            crypto_service,
            hub_identifier,
            server_url,
            api_key,
        }
    }

    /// Connect to Action Cable and start relaying messages.
    ///
    /// Returns:
    /// - `HubSender` - for sending terminal output to browser
    /// - `mpsc::Receiver<IdentifiedBrowserEvent>` - for receiving events from browser
    /// - `oneshot::Receiver<()>` - signals when the relay task exits
    pub async fn connect(
        self,
    ) -> Result<(
        HubSender,
        mpsc::Receiver<IdentifiedBrowserEvent>,
        oneshot::Receiver<()>,
    )> {
        let (event_tx, event_rx) = mpsc::channel::<IdentifiedBrowserEvent>(100);
        let (sender, shutdown_rx) = self.connect_with_event_channel(event_tx).await?;
        Ok((sender, event_rx, shutdown_rx))
    }

    /// Connect with an external event channel.
    pub async fn connect_with_event_channel(
        self,
        event_tx: mpsc::Sender<IdentifiedBrowserEvent>,
    ) -> Result<(HubSender, oneshot::Receiver<()>)> {
        let (output_tx, output_rx) = mpsc::channel::<OutputMessage>(100);
        let connected = Arc::new(RwLock::new(false));
        let output_sender = HubSender {
            tx: output_tx,
            connected: Arc::clone(&connected),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Build channel with E2E encryption and reliable delivery
        let mut channel = ActionCableChannel::builder()
            .server_url(&self.server_url)
            .api_key(&self.api_key)
            .crypto_service(self.crypto_service.clone())
            .reliable(true)
            .build();

        // Connect to HubChannel
        channel
            .connect(ChannelConfig {
                channel_name: "HubChannel".to_string(),
                hub_id: self.hub_identifier.clone(),
                agent_index: None,
                pty_index: None, // Hub channel doesn't use PTY index
                encrypt: true,
                compression_threshold: Some(4096),
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect: {}", e))?;

        // Spawn relay task
        let hub_identifier = self.hub_identifier;
        let server_url = self.server_url;
        let crypto_service = self.crypto_service;

        tokio::spawn(async move {
            let _shutdown_guard = scopeguard::guard(shutdown_tx, |tx| {
                let _ = tx.send(());
            });

            Self::run_relay_loop(
                channel,
                crypto_service,
                hub_identifier,
                server_url,
                output_rx,
                connected,
                event_tx,
            )
            .await;
        });

        Ok((output_sender, shutdown_rx))
    }

    /// Main relay loop - handles incoming/outgoing messages via channel.
    async fn run_relay_loop(
        mut channel: ActionCableChannel,
        crypto_service: CryptoServiceHandle,
        hub_identifier: String,
        server_url: String,
        mut output_rx: mpsc::Receiver<OutputMessage>,
        connected: Arc<RwLock<bool>>,
        event_tx: mpsc::Sender<IdentifiedBrowserEvent>,
    ) {
        let mut browser_identities: HashSet<String> = HashSet::new();

        loop {
            tokio::select! {
                // Handle outgoing messages
                Some(output_msg) = output_rx.recv() => {
                    match output_msg {
                        OutputMessage::RegenerateBundle => {
                            Self::handle_regenerate_bundle(&crypto_service, &event_tx).await;
                        }
                        _ if *connected.read().await && !browser_identities.is_empty() => {
                            Self::handle_output(
                                &channel,
                                output_msg,
                                &browser_identities,
                            ).await;
                        }
                        _ => {} // Drop if not connected
                    }
                }

                // Handle incoming messages from channel
                result = channel.recv() => {
                    match result {
                        Ok(msg) => {
                            Self::handle_incoming(
                                &channel,
                                &crypto_service,
                                &hub_identifier,
                                &server_url,
                                msg,
                                &connected,
                                &event_tx,
                                &mut browser_identities,
                            ).await;
                        }
                        Err(e) => {
                            log::warn!("Channel receive error: {}", e);
                            // Channel handles reconnection internally
                        }
                    }
                }
            }
        }
    }

    /// Handle bundle regeneration request.
    async fn handle_regenerate_bundle(
        crypto_service: &CryptoServiceHandle,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
    ) {
        log::info!("Regenerating PreKeyBundle on request");
        match crypto_service
            .get_prekey_bundle(crypto_service.next_prekey_id().await.unwrap_or(1))
            .await
        {
            Ok(bundle) => {
                log::info!(
                    "New PreKeyBundle generated with PreKey {}",
                    bundle.prekey_id.unwrap_or(0)
                );
                let _ = event_tx
                    .send((BrowserEvent::BundleRegenerated { bundle }, String::new()))
                    .await;
            }
            Err(e) => {
                log::error!("Failed to regenerate PreKeyBundle: {}", e);
            }
        }
    }

    /// Handle outgoing message to browser(s).
    async fn handle_output(
        channel: &ActionCableChannel,
        output_msg: OutputMessage,
        browser_identities: &HashSet<String>,
    ) {
        let (output, targets): (String, Vec<&String>) = match &output_msg {
            OutputMessage::Broadcast(data) => (data.clone(), browser_identities.iter().collect()),
            OutputMessage::Targeted { identity, data } => {
                if browser_identities.contains(identity) {
                    (data.clone(), vec![identity])
                } else {
                    log::warn!("Targeted send to unknown identity: {}", identity);
                    return;
                }
            }
            OutputMessage::RegenerateBundle => return,
        };

        let message = if let Ok(parsed) = serde_json::from_str::<TerminalMessage>(&output) {
            parsed
        } else {
            TerminalMessage::Output { data: output }
        };

        let payload = match serde_json::to_vec(&message) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to serialize message: {}", e);
                return;
            }
        };

        for identity in targets {
            if let Err(e) = channel
                .send_to(&payload, &PeerId::from(identity.as_str()))
                .await
            {
                log::error!("Failed to send to {}: {}", identity, e);
            }
        }
    }

    /// Handle incoming message from channel.
    #[allow(clippy::too_many_arguments)]
    async fn handle_incoming(
        channel: &ActionCableChannel,
        crypto_service: &CryptoServiceHandle,
        hub_identifier: &str,
        server_url: &str,
        msg: IncomingMessage,
        connected: &Arc<RwLock<bool>>,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
        browser_identities: &mut HashSet<String>,
    ) {
        let sender_identity = msg.sender.0.clone();

        // Track browser identity
        let is_new = browser_identities.insert(sender_identity.clone());
        if is_new {
            log::info!(
                "Browser connected: {} (total: {})",
                sender_identity,
                browser_identities.len()
            );
        }
        if !browser_identities.is_empty() {
            *connected.write().await = true;
        }

        // Parse command from payload
        log::debug!(
            "Received message from peer {} ({} bytes)",
            sender_identity,
            msg.payload.len()
        );
        let cmd: BrowserCommand = match serde_json::from_slice(&msg.payload) {
            Ok(c) => {
                log::debug!("Parsed command: {:?}", c);
                c
            }
            Err(e) => {
                if let Ok(text) = String::from_utf8(msg.payload.clone()) {
                    log::warn!("Failed to parse command from text: {} - error: {}", text, e);
                } else {
                    log::warn!("Failed to parse command (binary): {}", e);
                }
                return;
            }
        };

        Self::handle_browser_command(
            channel,
            crypto_service,
            hub_identifier,
            server_url,
            cmd,
            &sender_identity,
            event_tx,
        )
        .await;
    }

    /// Handle a parsed browser command.
    #[allow(clippy::too_many_arguments)]
    async fn handle_browser_command(
        channel: &ActionCableChannel,
        crypto_service: &CryptoServiceHandle,
        hub_identifier: &str,
        server_url: &str,
        cmd: BrowserCommand,
        sender_identity: &str,
        event_tx: &mpsc::Sender<IdentifiedBrowserEvent>,
    ) {
        let event = match cmd {
            BrowserCommand::Handshake { device_name, .. } => {
                log::info!("Browser handshake from: {}", device_name);

                // Send handshake_ack
                let ack = serde_json::json!({
                    "type": "handshake_ack",
                    "cli_version": env!("CARGO_PKG_VERSION"),
                    "hub_id": hub_identifier,
                });
                Self::send_to_browser(channel, sender_identity, &ack).await;

                if let Err(e) = event_tx
                    .send((
                        BrowserEvent::Connected {
                            public_key: sender_identity.to_string(),
                            device_name,
                        },
                        sender_identity.to_string(),
                    ))
                    .await
                {
                    log::error!("Failed to send event: {}", e);
                }
                return;
            }
            BrowserCommand::GenerateInvite => {
                log::info!("Browser requested invite bundle");

                match crypto_service.get_prekey_bundle(2).await {
                    Ok(bundle) => {
                        let bundle_bytes = bundle
                            .to_binary()
                            .expect("PreKeyBundle binary serializable");
                        let bundle_encoded = BASE32_NOPAD.encode(&bundle_bytes);
                        let invite_url =
                            format!("{}/hubs/{}#{}", server_url, hub_identifier, bundle_encoded);

                        let response = TerminalMessage::InviteBundle {
                            bundle: bundle_encoded,
                            url: invite_url,
                        };
                        Self::send_to_browser(channel, sender_identity, &response).await;
                    }
                    Err(e) => {
                        log::error!("Failed to generate invite bundle: {}", e);
                        let error_msg = TerminalMessage::Error {
                            message: format!("Failed to generate invite: {}", e),
                        };
                        Self::send_to_browser(channel, sender_identity, &error_msg).await;
                    }
                }
                return;
            }
            BrowserCommand::Input { data } => BrowserEvent::Input(data),
            BrowserCommand::SetMode { mode } => BrowserEvent::SetMode { mode },
            BrowserCommand::ListAgents => BrowserEvent::ListAgents,
            BrowserCommand::ListWorktrees => BrowserEvent::ListWorktrees,
            BrowserCommand::SelectAgent { id } => BrowserEvent::SelectAgent { id },
            BrowserCommand::CreateAgent {
                issue_or_branch,
                prompt,
            } => BrowserEvent::CreateAgent {
                issue_or_branch,
                prompt,
            },
            BrowserCommand::ReopenWorktree {
                path,
                branch,
                prompt,
            } => BrowserEvent::ReopenWorktree {
                path,
                branch,
                prompt,
            },
            BrowserCommand::DeleteAgent {
                id,
                delete_worktree,
            } => BrowserEvent::DeleteAgent {
                id,
                delete_worktree: delete_worktree.unwrap_or(false),
            },
            BrowserCommand::TogglePtyView => BrowserEvent::TogglePtyView,
            BrowserCommand::Scroll { direction, lines } => BrowserEvent::Scroll {
                direction,
                lines: lines.unwrap_or(3),
            },
            BrowserCommand::ScrollToBottom => BrowserEvent::ScrollToBottom,
            BrowserCommand::ScrollToTop => BrowserEvent::ScrollToTop,
            BrowserCommand::ConnectToPty {
                agent_index,
                pty_index,
            } => BrowserEvent::ConnectToPty {
                agent_index,
                pty_index,
            },
            BrowserCommand::Resize { cols, rows } => {
                BrowserEvent::Resize(BrowserResize { cols, rows })
            }
        };

        if let Err(e) = event_tx.send((event, sender_identity.to_string())).await {
            log::error!("Failed to forward event: {}", e);
        }
    }

    /// Send a message to a specific browser.
    async fn send_to_browser<T: Serialize>(
        channel: &ActionCableChannel,
        recipient_identity: &str,
        message: &T,
    ) {
        let bytes = match serde_json::to_vec(message) {
            Ok(b) => b,
            Err(e) => {
                log::error!("Failed to serialize message: {}", e);
                return;
            }
        };

        log::debug!(
            "Sending {} bytes to browser {}, peers: {:?}",
            bytes.len(),
            recipient_identity,
            channel.peers()
        );

        if let Err(e) = channel
            .send_to(&bytes, &PeerId::from(recipient_identity))
            .await
        {
            log::error!("Failed to send to {}: {}", recipient_identity, e);
        } else {
            log::debug!("Successfully queued message for {}", recipient_identity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hub_sender_debug() {
        // Just ensure Debug impl compiles
        let (tx, _rx) = mpsc::channel(1);
        let sender = HubSender {
            tx,
            connected: Arc::new(RwLock::new(false)),
        };
        let _ = format!("{:?}", sender);
    }

    #[test]
    fn test_hub_relay_debug() {
        // Ensure Debug impl compiles - can't create full HubRelay without CryptoServiceHandle
        // but we can test the struct layout
        assert!(std::mem::size_of::<HubRelay>() > 0);
    }
}
