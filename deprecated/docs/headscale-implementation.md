# Headscale Implementation Plan

**Goal**: Replace Olm + Action Cable with Headscale/Tailscale for E2E encrypted browser-to-CLI connectivity.

**Principle**: Keep connection_controller.js semantics, replace innards with Tailscale.

---

## Phase 1: Local Development Setup

### 1.1 Download tsconnect WASM
- [ ] Clone tailscale repo or download pre-built WASM
- [ ] Copy `wasm_exec.js` to `public/wasm/`
- [ ] Copy `main.wasm` (tsconnect) to `public/wasm/`
- [ ] Verify WASM loads in browser

### 1.2 Local Headscale Server
- [ ] Create `docker-compose.dev.yml` with Headscale
- [ ] Create `config/headscale/config.dev.yaml`
- [ ] Update `Procfile.dev` to start Headscale
- [ ] Verify Headscale starts and is accessible
- [ ] Create initial API key for development

### 1.3 Local Tailscale Setup
- [ ] Install Tailscale on dev machine (if not present)
- [ ] Document local testing setup in README

---

## Phase 2: Rails Backend (Minimal)

### 2.1 Headscale Client Service
- [ ] Create `app/services/headscale_client.rb`
- [ ] Implement `create_user(namespace)` method
- [ ] Implement `create_preauth_key(user:, ephemeral:, expiration:, tags:)` method
- [ ] Add credentials for Headscale API key
- [ ] Write tests for HeadscaleClient

### 2.2 User Model Updates
- [ ] Add migration for `headscale_namespace` column (or use computed)
- [ ] Add `after_create :create_headscale_namespace` callback
- [ ] Add `headscale_namespace` method
- [ ] Test namespace creation on user signup

### 2.3 Hub Model Updates
- [ ] Add migration for `tailscale_preauth_key` (encrypted)
- [ ] Add `cli_public_key` column for encrypted key exchange
- [ ] Update `before_create` to generate Tailscale pre-auth key
- [ ] Test hub creation includes Tailscale key

### 2.4 API Endpoint for Browser Keys
- [ ] Create `app/controllers/api/hubs/tailscale_controller.rb`
- [ ] Implement `POST /api/hubs/:hub_id/tailscale/browser_key`
- [ ] Encrypt response with CLI's public key
- [ ] Add route
- [ ] Test endpoint

---

## Phase 3: CLI Integration

### 3.1 Remove Old Relay Code
- [ ] Delete `cli/src/relay/olm.rs`
- [ ] Delete `cli/src/relay/connection.rs`
- [ ] Delete `cli/src/relay/persistence.rs`
- [ ] Update `cli/src/relay/mod.rs` to remove old exports
- [ ] Remove `vodozemac` from Cargo.toml
- [ ] Remove `tokio-tungstenite` if only for Action Cable
- [ ] Verify CLI still compiles

### 3.2 Tailscale Module
- [ ] Create `cli/src/tailscale.rs`
- [ ] Implement `TailscaleClient::new(headscale_url)`
- [ ] Implement `up(preauth_key)` - shell out to `tailscale up`
- [ ] Implement `hostname()` - get tailnet hostname
- [ ] Implement `is_connected()` - check status
- [ ] Add to `cli/src/lib.rs` or main module

### 3.3 Browser Connector
- [ ] Create `cli/src/browser_connect.rs`
- [ ] Implement key request from Rails API
- [ ] Implement key decryption (NaCl or similar)
- [ ] Implement `generate_connect_url()` with `#fragment`
- [ ] Implement `generate_qr_code()` using qrcode crate
- [ ] Add `qrcode` to Cargo.toml

### 3.4 Hub Startup Integration
- [ ] Update `cli/src/hub/mod.rs` to use TailscaleClient
- [ ] Join tailnet on startup if not connected
- [ ] Display QR code after connection
- [ ] Store tailnet hostname for display

### 3.5 Agent tmux Sessions
- [ ] Update `cli/src/agent.rs` to start agents in tmux
- [ ] Implement `start_in_tmux()` method
- [ ] Implement `tmux_session_name()` method
- [ ] Test agent starts in detached tmux session

---

## Phase 4: Browser Integration

### 4.1 Tailscale Wrapper Module
- [ ] Create `app/javascript/tailscale/index.js`
- [ ] Implement `loadTsconnect()` - load WASM
- [ ] Implement `TailscaleSession` class
- [ ] Implement `connect(headscaleUrl, preAuthKey)` method
- [ ] Implement `ssh(hostname, user, termConfig)` wrapper
- [ ] Implement state change callbacks
- [ ] Test WASM loading

### 4.2 Update Connection Controller
- [ ] Keep `connection_controller.js` structure
- [ ] Replace `initOlm()` with `loadTsconnect()`
- [ ] Replace `OlmSession` with `TailscaleSession`
- [ ] Update `parseUrlFragment()` for `#key=` format
- [ ] Update `initializeConnection()` to join tailnet
- [ ] Update `send()` to use SSH stdin
- [ ] Remove IndexedDB Olm session storage
- [ ] Add IndexedDB for Tailscale state (if needed)
- [ ] Keep listener registration API unchanged
- [ ] Test connection flow

### 4.3 Update Terminal Display Controller
- [ ] Verify it still works with new connection controller
- [ ] Update any Olm-specific code
- [ ] Test terminal I/O

### 4.4 Update Hub View
- [ ] Update `app/views/hubs/show.html.erb`
- [ ] Add Headscale URL data attribute
- [ ] Update QR code prompt messaging
- [ ] Test view renders correctly

---

## Phase 5: Delete Legacy Code

### 5.1 Rails Cleanup
- [ ] Delete `app/channels/terminal_channel.rb`
- [ ] Delete `app/channels/tunnel_channel.rb`
- [ ] Delete `app/javascript/crypto/olm.js`
- [ ] Delete `app/javascript/wasm/vodozemac_wasm.js`
- [ ] Delete `public/wasm/vodozemac_wasm*`
- [ ] Update `config/importmap.rb` to remove old pins
- [ ] Remove Action Cable config if unused

### 5.2 Other Cleanup
- [ ] Delete `vodozemac-wasm/` directory
- [ ] Delete old docs (`signal-protocol-migration-paused.md`, etc.)
- [ ] Update `CLAUDE.md` with new architecture
- [ ] Update `README.md` with new architecture

---

## Phase 6: Testing & Polish

### 6.1 Local E2E Testing
- [ ] Start Headscale (docker-compose)
- [ ] Start Rails server
- [ ] Build and run CLI
- [ ] Generate QR code from CLI
- [ ] Open browser, scan QR
- [ ] Verify browser joins tailnet
- [ ] Verify SSH connection works
- [ ] Verify terminal I/O works
- [ ] Test with multiple browsers

### 6.2 Error Handling
- [ ] Handle Headscale connection failures
- [ ] Handle tailnet join failures
- [ ] Handle SSH connection failures
- [ ] Add reconnection logic
- [ ] User-friendly error messages

### 6.3 Production Setup
- [ ] Add Headscale to `config/deploy.yml` as accessory
- [ ] Create production Headscale config
- [ ] Document deployment process
- [ ] Test production deployment

---

## File Inventory

### Create
```
docker-compose.dev.yml
config/headscale/config.dev.yaml
app/services/headscale_client.rb
app/controllers/api/hubs/tailscale_controller.rb
app/javascript/tailscale/index.js
cli/src/tailscale.rs
cli/src/browser_connect.rs
public/wasm/wasm_exec.js
public/wasm/tsconnect.wasm
```

### Modify
```
Procfile.dev
config/routes.rb
config/importmap.rb
app/models/user.rb
app/models/hub.rb
app/javascript/controllers/connection_controller.js
app/views/hubs/show.html.erb
cli/Cargo.toml
cli/src/relay/mod.rs
cli/src/hub/mod.rs
cli/src/agent.rs
```

### Delete
```
vodozemac-wasm/                          (entire directory)
cli/src/relay/olm.rs
cli/src/relay/connection.rs
cli/src/relay/persistence.rs
app/channels/terminal_channel.rb
app/channels/tunnel_channel.rb
app/javascript/crypto/olm.js
app/javascript/wasm/vodozemac_wasm.js
public/wasm/vodozemac_wasm*
```

---

## Current Status

Phase: **1.1 - Download tsconnect WASM**
