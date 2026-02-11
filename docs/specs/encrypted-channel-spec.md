# Encrypted Channel Spec

**Status:** Draft
**Author:** Jason Conigliari
**Date:** 2026-01-16

## 1. Overview

This spec defines a unified Rust trait for ActionCable channels with optional Signal Protocol encryption and gzip compression. The primary goal is enabling E2E encrypted HTTP proxying for agent server previews, ensuring Rails cannot inspect traffic between browser and agent.

### 1.1 Goals

- Unified `Channel` trait for all ActionCable communication (encrypted and unencrypted)
- E2E encryption for PreviewChannel matching existing TerminalRelayChannel security model
- Signal session reuse across channels (single QR scan for both terminal and preview)
- Gzip compression for payloads exceeding 4KB threshold
- Agent owns its channel connections (not Hub)
- Architecture supports future WebSocket proxying without structural changes

### 1.2 Non-Goals

- WebSocket proxying (deferred to v2)
- Sharing previews with external collaborators (future permissions model)
- Changes to existing TunnelChannel behavior

## 2. Architecture

### 2.1 Thread Architecture (Signal as Service)

libsignal-protocol uses `!Send` futures for security (thread-local crypto state). Instead of
sharing `Arc<SignalProtocolManager>` across threads, we use a dedicated crypto service thread
with message-passing:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              HUB                                         │
│                                                                          │
│   ┌─────────────────────────────────────────────────────────────────┐   │
│   │                     CRYPTO SERVICE THREAD                        │   │
│   │                        (LocalSet)                                │   │
│   │                                                                  │   │
│   │   SignalProtocolManager                                          │   │
│   │   ├── identity_store     (persisted)                            │   │
│   │   ├── session_store      (browser sessions)                     │   │
│   │   └── prekey_bundle      (for QR code)                          │   │
│   │                                                                  │   │
│   │   Processes: Encrypt, Decrypt, HasSession, GetPreKeyBundle, ... │   │
│   └──────────────────────────┬──────────────────────────────────────┘   │
│                              │                                           │
│              ┌───────────────┼───────────────┐                          │
│              │ CryptoServiceHandle           │  (Send + Clone)          │
│              │   .encrypt()                  │                          │
│              │   .decrypt()                  │                          │
│              │   .has_session()              │                          │
│              └───────────────┬───────────────┘                          │
│                              │                                           │
│   ┌──────────▼─────┐  ┌──────▼──────┐  ┌─────▼─────┐                   │
│   │ TERMINAL RELAY │  │ PREVIEW     │  │ PREVIEW   │                   │
│   │    THREAD      │  │ RELAY 0     │  │ RELAY N   │                   │
│   │                │  │             │  │           │                   │
│   │ - WebSocket    │  │ - WebSocket │  │ - ...     │                   │
│   │ - All agents   │  │ - HTTP proxy│  │           │                   │
│   │   terminal I/O │  │ - Agent 0   │  │           │                   │
│   └────────────────┘  └─────────────┘  └───────────┘                   │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Connection Topology

```
Hub
├── crypto_service: CryptoServiceHandle   (shared handle to crypto thread)
├── tunnel_channel: ActionCableChannel    (unencrypted, general purpose)
├── terminal_relay: TerminalRelay         (uses crypto_service)
├── preview_relays: Vec<PreviewRelay>     (one per agent, uses crypto_service)
└── agents: Vec<Agent>
    └── Agent[n]
        ├── init_pty: PtySession          (terminal output → TerminalRelay)
        └── server_pty: PtySession        (HTTP proxy → PreviewRelay[n])
```

### 2.3 Channel Summary

| Channel | Thread | Encryption | Purpose |
|---------|--------|------------|---------|
| TunnelChannel | Hub main | None | General hub communication |
| TerminalRelayChannel | Terminal Relay | Signal (via CryptoService) | Terminal I/O for all agents |
| PreviewChannel | Preview Relay N | Signal (via CryptoService) | HTTP proxying for agent N |

### 2.4 Stream Structure

Both encrypted channels use the same two-stream pattern:

```
TerminalRelayChannel:
  CLI subscribes to     → terminal_relay:{hub_id}:cli
  Browser subscribes to → terminal_relay:{hub_id}:browser:{identity}

PreviewChannel:
  Agent subscribes to   → preview:{hub_id}:{agent_index}:agent
  Browser subscribes to → preview:{hub_id}:{agent_index}:browser:{identity}
```

### 2.5 Signal Session Reuse

The `CryptoService` centralizes all Signal operations. When the same browser identity connects
to multiple channels, the service automatically reuses the existing Signal session. No
additional QR scan required.

```
Browser Identity X connects to TerminalRelayChannel
  → TerminalRelay calls crypto_service.decrypt()
  → CryptoService creates Signal session for identity X

Browser Identity X connects to PreviewChannel
  → PreviewRelay calls crypto_service.has_session("X") → true
  → Reuses session, no new handshake needed
```

### 2.6 CryptoService API

```rust
// cli/src/relay/crypto_service.rs

/// Handle for sending requests to the crypto service.
/// This is Send + Sync and can be cloned and shared across threads.
#[derive(Clone)]
pub struct CryptoServiceHandle { /* ... */ }

impl CryptoServiceHandle {
    pub async fn encrypt(&self, plaintext: &[u8], peer_identity: &str) -> Result<SignalEnvelope>;
    pub async fn decrypt(&self, envelope: &SignalEnvelope) -> Result<Vec<u8>>;
    pub async fn has_session(&self, peer_identity: &str) -> Result<bool>;
    pub async fn get_prekey_bundle(&self, preferred_id: u32) -> Result<PreKeyBundleData>;
    pub async fn group_encrypt(&self, plaintext: &[u8]) -> Result<SignalEnvelope>;
    // ... other methods
}

/// Start the crypto service in a dedicated thread.
pub fn start(hub_id: &str) -> Result<CryptoServiceHandle>;
```

## 3. Rust Channel Trait

### 3.1 Core Types

```rust
// cli/src/channel/mod.rs

#[derive(Clone)]
pub struct ChannelConfig {
    pub channel_name: String,
    pub hub_id: String,
    pub agent_index: Option<usize>,
    pub encrypt: bool,
    pub compression_threshold: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting { attempt: u32, next_retry_ms: u64 },
    Error(String),
}

#[derive(Debug)]
pub struct IncomingMessage {
    pub payload: Vec<u8>,
    pub sender: PeerId,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct PeerId(pub String);

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Send failed: {0}")]
    SendFailed(String),
    #[error("Encryption error: {0}")]
    EncryptionError(String),
    #[error("Decryption error: {0}")]
    DecryptionError(String),
    #[error("Compression error: {0}")]
    CompressionError(String),
    #[error("Channel closed")]
    Closed,
}
```

### 3.2 Channel Trait

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    /// Connect to the ActionCable channel
    async fn connect(&mut self, config: ChannelConfig) -> Result<(), ChannelError>;

    /// Disconnect and clean up
    async fn disconnect(&mut self);

    /// Current connection state
    fn state(&self) -> ConnectionState;

    /// Send message to all connected peers (encrypted channels)
    /// or broadcast (unencrypted channels)
    async fn send(&self, msg: &[u8]) -> Result<(), ChannelError>;

    /// Send message to specific peer
    async fn send_to(&self, msg: &[u8], peer: &PeerId) -> Result<(), ChannelError>;

    /// Receive next message (blocks until available)
    async fn recv(&mut self) -> Result<IncomingMessage, ChannelError>;

    /// List connected peers
    fn peers(&self) -> Vec<PeerId>;

    /// Check if peer has active session
    fn has_peer(&self, peer: &PeerId) -> bool;
}
```

### 3.3 ActionCable Implementation

```rust
// cli/src/channel/action_cable.rs

pub struct ActionCableChannel {
    config: Option<ChannelConfig>,
    state: Arc<RwLock<ConnectionState>>,
    signal_manager: Option<Arc<SignalProtocolManager>>,

    write: Option<SplitSink<WebSocketStream, Message>>,
    incoming_rx: mpsc::Receiver<IncomingMessage>,

    backoff: ExponentialBackoff,
    last_activity: Instant,

    peers: Arc<RwLock<HashSet<PeerId>>>,
}

impl ActionCableChannel {
    /// Create encrypted channel with shared SignalProtocolManager
    pub fn encrypted(signal_manager: Arc<SignalProtocolManager>) -> Self;

    /// Create unencrypted channel
    pub fn unencrypted() -> Self;
}
```

## 4. Compression

### 4.1 Threshold

Compression is applied to payloads exceeding 4KB. Payloads already compressed (detected via Content-Encoding header for HTTP responses) are not double-compressed.

### 4.2 Wire Format

Single marker byte prefix:
- `0x00` — Uncompressed, payload follows directly
- `0x1f` — Gzip compressed, decompress remaining bytes

### 4.3 Implementation

```rust
fn maybe_compress(&self, data: &[u8]) -> Result<Vec<u8>, ChannelError> {
    let threshold = match self.config.as_ref().and_then(|c| c.compression_threshold) {
        Some(t) => t,
        None => return Ok(data.to_vec()),
    };

    if data.len() < threshold {
        let mut result = vec![0x00];
        result.extend_from_slice(data);
        return Ok(result);
    }

    let mut compressed = vec![0x1f];
    let mut encoder = GzEncoder::new(&mut compressed, Compression::fast());
    encoder.write_all(data)?;
    encoder.finish()?;

    if compressed.len() < data.len() + 1 {
        Ok(compressed)
    } else {
        let mut result = vec![0x00];
        result.extend_from_slice(data);
        Ok(result)
    }
}

fn maybe_decompress(&self, data: &[u8]) -> Result<Vec<u8>, ChannelError> {
    if data.is_empty() {
        return Ok(vec![]);
    }

    match data[0] {
        0x00 => Ok(data[1..].to_vec()),
        0x1f => {
            let mut decoder = GzDecoder::new(&data[1..]);
            let mut result = Vec::new();
            decoder.read_to_end(&mut result)?;
            Ok(result)
        }
        _ => Err(ChannelError::CompressionError("Unknown marker".into())),
    }
}
```

### 4.4 HTTP Response Handling

```rust
fn should_compress_response(body: &[u8], headers: &[(String, String)], threshold: usize) -> bool {
    if body.len() < threshold {
        return false;
    }

    let already_compressed = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("content-encoding") &&
        (v.contains("gzip") || v.contains("br") || v.contains("deflate") || v.contains("zstd"))
    });

    !already_compressed
}
```

## 5. Rails Channels

### 5.1 PreviewChannel

```ruby
# app/channels/preview_channel.rb
class PreviewChannel < ApplicationCable::Channel
  def subscribed
    @hub_id = params[:hub_id]
    @agent_index = params[:agent_index]
    @is_browser = params[:browser_identity].present?
    @browser_identity = params[:browser_identity]

    if @is_browser
      stream_from browser_stream
    else
      stream_from agent_stream
    end
  end

  def relay(data)
    if @is_browser
      ActionCable.server.broadcast(agent_stream, data)
    else
      identity = data["recipient_identity"]
      ActionCable.server.broadcast(browser_stream_for(identity), data)
    end
  end

  private

  def agent_stream
    "preview:#{@hub_id}:#{@agent_index}:agent"
  end

  def browser_stream
    browser_stream_for(@browser_identity)
  end

  def browser_stream_for(identity)
    "preview:#{@hub_id}:#{@agent_index}:browser:#{identity}"
  end
end
```

## 6. Message Protocol

### 6.1 Preview Messages (Encrypted Payload)

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PreviewMessage {
    HttpRequest {
        request_id: String,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    },
    HttpResponse {
        request_id: String,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },

    // Reserved for future WebSocket proxying
    WsOpen { tunnel_id: String, url: String, protocols: Vec<String> },
    WsMessage { tunnel_id: String, data: WsData },
    WsClose { tunnel_id: String, code: Option<u16>, reason: Option<String> },
    WsError { tunnel_id: String, error: String },
}

#[derive(Serialize, Deserialize)]
pub enum WsData {
    Text(String),
    Binary(Vec<u8>),
}
```

## 7. Agent Integration

### 7.1 PtySession

```rust
pub struct PtySession {
    pub pty: Pty,
    pub channel: ActionCableChannel,
    pub channel_type: PtyChannelType,
}

pub enum PtyChannelType {
    Terminal,
    Preview,
}
```

### 7.2 Agent Lifecycle

```rust
impl Agent {
    pub async fn start(
        hub_id: String,
        agent_index: usize,
        signal_manager: Arc<SignalProtocolManager>,
        worktree_path: &Path,
    ) -> Result<Self> {
        let init_channel = ActionCableChannel::encrypted(signal_manager.clone());
        init_channel.connect(ChannelConfig {
            channel_name: "TerminalRelayChannel".into(),
            hub_id: hub_id.clone(),
            agent_index: Some(agent_index),
            encrypt: true,
            compression_threshold: Some(4096),
        }).await?;

        let init_pty = PtySession {
            pty: Pty::spawn("botster_init", worktree_path)?,
            channel: init_channel,
            channel_type: PtyChannelType::Terminal,
        };

        let server_channel = ActionCableChannel::encrypted(signal_manager.clone());
        server_channel.connect(ChannelConfig {
            channel_name: "PreviewChannel".into(),
            hub_id: hub_id.clone(),
            agent_index: Some(agent_index),
            encrypt: true,
            compression_threshold: Some(4096),
        }).await?;

        let server_pty = PtySession {
            pty: Pty::spawn("botster_server", worktree_path)?,
            channel: server_channel,
            channel_type: PtyChannelType::Preview,
        };

        Ok(Self {
            id: agent_index,
            hub_id,
            init_pty,
            server_pty,
            signal_manager,
        })
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.init_pty.channel.disconnect().await;
        self.init_pty.pty.kill()?;
        self.server_pty.channel.disconnect().await;
        self.server_pty.pty.kill()?;
        Ok(())
    }
}
```

### 7.3 HTTP Request Handling

```rust
impl Agent {
    pub async fn handle_preview_message(
        &mut self,
        msg: PreviewMessage,
        sender: PeerId,
    ) -> Result<()> {
        match msg {
            PreviewMessage::HttpRequest { request_id, method, path, headers, body } => {
                let port = self.server_pty.port.ok_or(AgentError::ServerNotRunning)?;
                let response = self.forward_http_request(port, &method, &path, &headers, body.as_deref()).await?;

                let response_msg = PreviewMessage::HttpResponse {
                    request_id,
                    status: response.status,
                    headers: response.headers,
                    body: response.body,
                };

                self.server_pty.channel.send_to(
                    &serde_json::to_vec(&response_msg)?,
                    &sender,
                ).await?;
            }
            _ => { /* WebSocket messages - not implemented */ }
        }
        Ok(())
    }
}
```

## 8. Browser Architecture

### 8.1 Shared Transport Module

```javascript
// app/javascript/transport/encrypted-channel.js

export class EncryptedChannel {
    constructor(hubId, channelName, agentIndex = null) {
        this.hubId = hubId;
        this.channelName = channelName;
        this.agentIndex = agentIndex;
        this.signalSession = null;
        this.subscription = null;
        this.listeners = new Map();
    }

    async connect() {
        this.signalSession = await SignalSession.load(this.hubId);
        if (!this.signalSession) {
            throw new Error('NO_SESSION');
        }

        this.subscription = consumer.subscriptions.create({
            channel: this.channelName,
            hub_id: this.hubId,
            agent_index: this.agentIndex,
            browser_identity: this.signalSession.identityKey,
        }, {
            received: (data) => this.handleReceived(data),
            disconnected: () => this.notifyListeners('disconnected'),
        });
    }

    async send(message) {
        const json = JSON.stringify(message);
        const compressed = this.maybeCompress(new TextEncoder().encode(json));
        const encrypted = await this.signalSession.encrypt(compressed);
        this.subscription.perform('relay', { envelope: encrypted });
    }

    on(event, callback) {
        if (!this.listeners.has(event)) {
            this.listeners.set(event, []);
        }
        this.listeners.get(event).push(callback);
    }
}
```

### 8.2 Preview Service Worker

```javascript
// app/javascript/preview/service-worker.js

let messageChannel = null;
const pendingRequests = new Map();

self.addEventListener('message', (event) => {
    if (event.data.type === 'INIT') {
        messageChannel = event.ports[0];
        messageChannel.onmessage = handleResponse;
    }
});

self.addEventListener('fetch', (event) => {
    if (shouldIntercept(event.request.url)) {
        event.respondWith(proxyRequest(event.request));
    }
});

async function proxyRequest(request) {
    const requestId = crypto.randomUUID();
    const body = request.body ? await request.arrayBuffer() : null;

    messageChannel.postMessage({
        type: 'HTTP_REQUEST',
        requestId,
        method: request.method,
        path: new URL(request.url).pathname + new URL(request.url).search,
        headers: [...request.headers.entries()],
        body: body ? Array.from(new Uint8Array(body)) : null,
    });

    return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
            pendingRequests.delete(requestId);
            reject(new Error('Request timeout'));
        }, 30000);

        pendingRequests.set(requestId, { resolve, reject, timeout });
    });
}

function handleResponse(event) {
    const { requestId, status, headers, body } = event.data;
    const pending = pendingRequests.get(requestId);

    if (pending) {
        clearTimeout(pending.timeout);
        pendingRequests.delete(requestId);
        pending.resolve(new Response(new Uint8Array(body), {
            status,
            headers: new Headers(headers),
        }));
    }
}
```

### 8.3 Preview Controller

```javascript
// app/javascript/controllers/preview_controller.js

import { Controller } from "@hotwired/stimulus";
import { EncryptedChannel } from "../transport/encrypted-channel";

export default class extends Controller {
    static values = { hubId: String, agentIndex: Number };

    async connect() {
        try {
            this.channel = new EncryptedChannel(
                this.hubIdValue,
                'PreviewChannel',
                this.agentIndexValue
            );

            this.channel.on('message', (msg) => this.handleMessage(msg));
            this.channel.on('error', (err) => this.handleError(err));

            await this.channel.connect();
            await this.initServiceWorker();
        } catch (e) {
            if (e.message === 'NO_SESSION') {
                this.showSessionRequired();
            } else {
                this.showError(e);
            }
        }
    }

    handleError(err) {
        if (err.type === 'DECRYPTION_ERROR') {
            this.showDecryptionError();
        }
    }

    showSessionRequired() {
        // Prompt user to connect via terminal first
    }

    showDecryptionError() {
        // Surface error, let user decide to retry or reconnect
    }
}
```

## 9. Error Handling

### 9.1 Decryption Errors

Decryption errors are surfaced to the user. Session is NOT automatically cleared.

**CLI:**
```rust
match channel.recv().await {
    Err(ChannelError::DecryptionError(e)) => {
        log::warn!("Decryption failed from {}: {}", sender, e);
        // Do not clear session - let user decide
    }
}
```

**Browser:**
```javascript
showDecryptionError() {
    this.showModal({
        title: 'Encryption Error',
        message: 'Failed to decrypt message. This may be a temporary issue.',
        actions: [
            { label: 'Retry', action: () => location.reload() },
            { label: 'Reconnect', action: () => this.redirectToTerminal() },
        ]
    });
}
```

### 9.2 Connection Errors

Exponential backoff with jitter:
- Initial: 1 second
- Maximum: 30 seconds
- Jitter: 0-1000ms random

### 9.3 Session Not Found

When browser connects to PreviewChannel without existing Signal session:
- Display message: "Please connect via terminal first to establish encrypted session"
- Provide link to terminal page with QR scanner

## 10. File Structure

```
cli/src/
├── channel/
│   ├── mod.rs              # Channel trait, types, errors
│   ├── action_cable.rs     # ActionCableChannel implementation
│   └── compression.rs      # Gzip with marker bytes
├── agent/
│   ├── mod.rs              # Agent struct with PtySessions
│   └── preview.rs          # HTTP request forwarding
└── hub/
    └── mod.rs              # Hub with tunnel_channel, agents

app/javascript/
├── transport/
│   └── encrypted-channel.js
├── preview/
│   └── service-worker.js
└── controllers/
    ├── connection_controller.js  # Refactor to use transport/
    └── preview_controller.js

app/channels/
├── terminal_relay_channel.rb
└── preview_channel.rb
```

## 11. Acceptance Criteria

### 11.1 Channel Trait

> Note: Original spec envisioned a `Channel` trait, but implementation uses direct struct-based
> approach for TerminalRelay and PreviewRelay. The "Signal as Service" architecture supersedes
> the trait-based design by providing a shared CryptoServiceHandle across all relays.

- [ ] `Channel` trait defined with `connect`, `disconnect`, `send`, `send_to`, `recv`, `peers`, `has_peer`
- [ ] `ActionCableChannel` implements `Channel` trait
- [ ] `ActionCableChannel::encrypted()` creates channel with Signal encryption
- [ ] `ActionCableChannel::unencrypted()` creates channel without encryption
- [x] Encryption/decryption handled transparently (via CryptoServiceHandle)
- [x] Compression applied when payload > 4KB threshold (HttpProxy)
- [x] Compression skipped for already-compressed content (Content-Encoding check)
- [x] Wire format uses marker byte prefix (0x00 uncompressed, 0x1f gzip)

### 11.2 Signal Session Reuse ("Signal as Service")

- [x] `CryptoService` runs SignalProtocolManager in dedicated thread with LocalSet
- [x] `CryptoServiceHandle` is Send + Clone, shared across all relays
- [x] TerminalRelay uses CryptoServiceHandle for encrypt/decrypt
- [x] PreviewRelay uses same CryptoServiceHandle (cloned)
- [x] Browser connecting to PreviewChannel with existing terminal session reuses Signal session
- [x] No additional QR scan required for preview access after terminal connection
- [x] Session lookup by browser identity via `has_session()`

### 11.3 Hub/Relay Ownership

> Note: Implementation differs from spec - Hub spawns PreviewRelay per-agent when tunnel_port
> is available, rather than Agent owning channels directly.

- [x] Hub starts single CryptoService on startup
- [x] Hub spawns TerminalRelay with CryptoServiceHandle
- [x] Hub spawns PreviewRelay per-agent when tunnel_port available
- [x] PreviewRelay handles HTTP proxying to agent's dev server
- [ ] Agent owns its `init_pty` (TerminalRelayChannel) and `server_pty` (PreviewChannel)
- [ ] Agent creates/connects channels during `start()`
- [ ] Agent disconnects/cleans up channels during `shutdown()`
- [x] Hub only owns TunnelChannel (unencrypted, general purpose)

### 11.4 PreviewChannel (Rails)

- [x] PreviewChannel accepts `hub_id`, `agent_index`, `browser_identity` params
- [x] Browser subscribers stream from `preview:{hub}:{agent}:browser:{identity}`
- [x] Agent subscribers stream from `preview:{hub}:{agent}:agent`
- [x] `relay` action routes browser→agent or agent→browser based on subscriber type
- [x] Rails never decrypts message content (dumb pipe)

### 11.5 HTTP Proxying

- [x] Browser service worker intercepts fetch requests to preview origin
- [x] Requests serialized and sent via encrypted PreviewChannel
- [x] CLI forwards request to local server port (127.0.0.1:{port}) via HttpProxy
- [x] Response encrypted and sent back to originating browser
- [x] Service worker constructs synthetic Response from decrypted payload
- [x] Hop-by-hop headers filtered (Connection, Keep-Alive, etc.)

### 11.6 Browser Integration

- [x] `EncryptedChannel` class extracted for reuse (PreviewChannel extends it)
- [x] Preview page loads Signal session from IndexedDB via SignalSession.load()
- [x] "No session" error shows user-friendly message
- [x] Decryption errors surfaced to user (not auto-cleared)
- [x] Service worker communicates with main thread via MessageChannel

### 11.7 Reconnection

- [x] Exponential backoff (1s initial, 30s max) with jitter
- [x] Connection state exposed via `getState()` / ChannelState enum
- [x] Peers tracked across reconnections (browser_identities HashSet)
- [x] Signal session state preserved across reconnections (CryptoService persists)

### 11.8 Error Handling

- [x] Decryption errors logged but session preserved
- [x] User presented with options on encryption failure (via handleError)
- [x] Request timeout (30s) in service worker with appropriate error response

## 12. Future Considerations

### 12.1 WebSocket Proxying (v2)

Message types reserved in protocol (`WsOpen`, `WsMessage`, `WsClose`, `WsError`). Implementation requires:
- WebSocket constructor patch in preview page (before app code loads)
- CLI maintains `HashMap<tunnel_id, WebSocketStream>` for active tunnels
- Bidirectional message forwarding through encrypted channel

Architecture unchanged - just additional message type handlers.

### 12.2 Preview Sharing

Future permission model for sharing previews with collaborators:
- Share tokens with scoped access (read-only, time-limited)
- PreviewChannel authorization check before stream subscription
- Separate Signal sessions per share recipient

### 12.3 Multiple Browsers

Current architecture supports multiple browsers with same identity:
- Each browser window loads session from IndexedDB
- Each maintains independent Double Ratchet state
- Signal Protocol handles via message counters (out-of-order OK, duplicates detected)
