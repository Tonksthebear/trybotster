# Encrypted Channel Implementation Plan

**Spec:** `docs/specs/encrypted-channel-spec.md`
**Status:** In Progress

## Phase 1: Rust Channel Trait Foundation

### 1.1 Create channel module structure
- [ ] Create `cli/src/channel/mod.rs` with trait definition
- [ ] Define `ChannelConfig`, `ConnectionState`, `IncomingMessage`, `PeerId`, `ChannelError`
- [ ] Define `Channel` trait with all methods
- [ ] Export from `cli/src/lib.rs`

### 1.2 Implement compression module
- [ ] Create `cli/src/channel/compression.rs`
- [ ] Implement `maybe_compress()` with 4KB threshold and marker byte
- [ ] Implement `maybe_decompress()` with marker byte detection
- [ ] Implement `should_compress_response()` for Content-Encoding check
- [ ] Add unit tests for compression/decompression round-trip
- [ ] Add unit tests for already-compressed content detection

### 1.3 Implement ActionCableChannel
- [ ] Create `cli/src/channel/action_cable.rs`
- [ ] Implement `ActionCableChannel` struct with all fields
- [ ] Implement `ActionCableChannel::encrypted(signal_manager)` constructor
- [ ] Implement `ActionCableChannel::unencrypted()` constructor
- [ ] Implement `Channel::connect()` - WebSocket connection + ActionCable subscription
- [ ] Implement `Channel::disconnect()` - Clean shutdown
- [ ] Implement `Channel::state()` - Return current ConnectionState
- [ ] Implement `Channel::send()` - Compress, encrypt (if configured), send to all peers
- [ ] Implement `Channel::send_to()` - Compress, encrypt, send to specific peer
- [ ] Implement `Channel::recv()` - Receive, decrypt (if configured), decompress
- [ ] Implement `Channel::peers()` - Return list of connected peers
- [ ] Implement `Channel::has_peer()` - Check if peer has session
- [ ] Implement reconnection logic with exponential backoff (1s-30s) and jitter
- [ ] Implement health check / stale connection detection

## Phase 2: Agent Ownership Refactor

### 2.1 Create PtySession struct
- [ ] Define `PtySession` struct in `cli/src/agent/mod.rs`
- [ ] Include `pty: Pty`, `channel: ActionCableChannel`, `channel_type: PtyChannelType`
- [ ] Define `PtyChannelType` enum (Terminal, Preview)

### 2.2 Refactor Agent to own channels
- [ ] Update `Agent` struct to have `init_pty: PtySession` and `server_pty: PtySession`
- [ ] Update `Agent::start()` to:
  - Accept `signal_manager: Arc<SignalProtocolManager>`
  - Create and connect TerminalRelayChannel for init_pty
  - Create and connect PreviewChannel for server_pty
  - Spawn both PTY processes
- [ ] Update `Agent::shutdown()` to disconnect both channels and kill PTYs
- [ ] Update any agent message handling to use new channel abstraction

### 2.3 Update Hub
- [ ] Remove channel management from Hub (keep only TunnelChannel)
- [ ] Pass `signal_manager` to Agent during spawn
- [ ] Update agent iteration/routing to work with new structure

### 2.4 Migrate existing TerminalRelayChannel usage
- [ ] Identify all places currently using relay connection
- [ ] Refactor to use new `Channel` trait via agent's `init_pty.channel`
- [ ] Ensure backward compatibility during migration
- [ ] Remove old relay code once migration complete

## Phase 3: Preview Message Protocol

### 3.1 Define message types
- [ ] Create `cli/src/channel/preview_messages.rs` (or in agent/preview.rs)
- [ ] Define `PreviewMessage` enum with HttpRequest, HttpResponse variants
- [ ] Define reserved WebSocket variants (WsOpen, WsMessage, WsClose, WsError)
- [ ] Define `WsData` enum (Text, Binary)
- [ ] Implement Serialize/Deserialize

### 3.2 Implement HTTP forwarding
- [ ] Create `cli/src/agent/preview.rs`
- [ ] Implement `Agent::handle_preview_message()`
- [ ] Implement `forward_http_request()` using reqwest
- [ ] Filter hop-by-hop headers (Connection, Keep-Alive, etc.)
- [ ] Handle request timeout
- [ ] Send encrypted response back to originating browser via `send_to()`

### 3.3 Wire up preview message loop
- [ ] Add message receive loop for server_pty channel
- [ ] Deserialize incoming PreviewMessage
- [ ] Route to handle_preview_message()
- [ ] Handle errors (log, don't crash)

## Phase 4: Rails PreviewChannel

### 4.1 Create PreviewChannel
- [ ] Create `app/channels/preview_channel.rb`
- [ ] Accept params: `hub_id`, `agent_index`, `browser_identity`
- [ ] Detect browser vs agent subscriber
- [ ] Browser streams from `preview:{hub}:{agent}:browser:{identity}`
- [ ] Agent streams from `preview:{hub}:{agent}:agent`
- [ ] Implement `relay` action routing browser↔agent

### 4.2 Add routes/authentication
- [ ] Ensure ActionCable authentication works for PreviewChannel
- [ ] Verify hub ownership / access permissions
- [ ] Add any necessary route configuration

### 4.3 Test Rails channel
- [ ] Write channel test for browser subscription
- [ ] Write channel test for agent subscription
- [ ] Write channel test for relay routing
- [ ] Test with multiple browsers

## Phase 5: Browser Transport Module

### 5.1 Extract EncryptedChannel class
- [ ] Create `app/javascript/transport/encrypted-channel.js`
- [ ] Move Signal session loading from connection_controller
- [ ] Implement `connect()` - load session, subscribe to channel
- [ ] Implement `send()` - compress, encrypt, relay
- [ ] Implement `handleReceived()` - decrypt, decompress, notify
- [ ] Implement `maybeCompress()` with 4KB threshold and marker byte
- [ ] Implement `maybeDecompress()` with marker detection
- [ ] Implement listener registration (`on()`, `notifyListeners()`)
- [ ] Handle NO_SESSION error case

### 5.2 Refactor connection_controller
- [ ] Import and use EncryptedChannel
- [ ] Remove duplicated encryption/compression logic
- [ ] Keep UI rendering, status display
- [ ] Ensure backward compatibility

### 5.3 Add pako for browser-side gzip
- [ ] Add pako to importmap or package.json
- [ ] Verify gzip/gunzip works in browser context

## Phase 6: Preview Service Worker

### 6.1 Create service worker
- [ ] Create `app/javascript/preview/service-worker.js`
- [ ] Implement `message` event handler for INIT (receive MessagePort)
- [ ] Implement `fetch` event handler
- [ ] Implement `shouldIntercept()` to filter requests
- [ ] Implement `proxyRequest()` - serialize request, post to main thread
- [ ] Implement `handleResponse()` - construct synthetic Response
- [ ] Add 30s request timeout
- [ ] Handle pending request cleanup

### 6.2 Service worker registration
- [ ] Create registration/bootstrap logic
- [ ] Ensure SW is scoped to preview paths
- [ ] Handle SW updates gracefully

## Phase 7: Preview Controller (Stimulus)

### 7.1 Create preview_controller.js
- [ ] Create `app/javascript/controllers/preview_controller.js`
- [ ] Define values: hubId, agentIndex
- [ ] In `connect()`:
  - Create EncryptedChannel for PreviewChannel
  - Register message/error listeners
  - Connect channel
  - Initialize service worker
- [ ] Implement `initServiceWorker()`:
  - Register SW
  - Create MessageChannel
  - Send INIT with port to SW
  - Set up message forwarding
- [ ] Implement `handleMessage()` - route HttpResponse to SW
- [ ] Implement error handlers:
  - `showSessionRequired()` for NO_SESSION
  - `showDecryptionError()` for DECRYPTION_ERROR

### 7.2 Create preview view/page
- [ ] Create preview route in Rails
- [ ] Create preview view template
- [ ] Wire up preview_controller with data attributes
- [ ] Add iframe or content container for rendered preview

## Phase 8: Integration & Testing

### 8.1 End-to-end flow testing
- [ ] Test: Terminal connection establishes Signal session
- [ ] Test: Preview connection reuses existing Signal session
- [ ] Test: HTTP request flows browser → SW → main → channel → CLI → localhost
- [ ] Test: HTTP response flows back correctly
- [ ] Test: Large response triggers compression
- [ ] Test: Already-compressed response not double-compressed
- [ ] Test: Multiple browsers can connect simultaneously
- [ ] Test: Targeted responses go to correct browser

### 8.2 Error case testing
- [ ] Test: Preview without terminal session shows helpful error
- [ ] Test: Decryption error surfaces to user (not auto-cleared)
- [ ] Test: Connection drop triggers reconnection with backoff
- [ ] Test: Request timeout handled gracefully

### 8.3 CLI integration tests
- [ ] Add integration test for Channel trait
- [ ] Add integration test for Agent with both PTY sessions
- [ ] Add integration test for preview message handling

## Phase 9: Cleanup & Documentation

### 9.1 Remove deprecated code
- [ ] Remove old relay implementation (once migrated)
- [ ] Remove any unused tunnel code
- [ ] Clean up dead imports

### 9.2 Update documentation
- [ ] Update CLAUDE.md if architecture section needs changes
- [ ] Add inline documentation to Channel trait
- [ ] Document PreviewChannel usage in code comments

---

## Implementation Order

Recommended sequence to minimize integration risk:

1. **Phase 1** (Rust foundation) - Can be developed independently
2. **Phase 3.1** (Message types) - Needed for protocol definition
3. **Phase 4** (Rails channel) - Can test with manual WebSocket
4. **Phase 2** (Agent refactor) - Major change, do carefully
5. **Phase 5** (Browser transport) - Extract and refactor
6. **Phase 6** (Service worker) - New code, isolated
7. **Phase 7** (Preview controller) - Brings it together
8. **Phase 3.2-3.3** (HTTP forwarding) - Complete the loop
9. **Phase 8** (Testing) - Validate everything
10. **Phase 9** (Cleanup) - Polish

## Notes

- Keep existing TerminalRelayChannel working throughout migration
- Test incrementally - don't wait until the end
- Signal session reuse is the key feature - verify early in Phase 2
