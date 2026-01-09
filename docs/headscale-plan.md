# Headscale Browser Connectivity - Implementation Plan

**Date**: 2025-01-08
**Status**: Ready for implementation

---

## Goals

1. **E2E encrypted** browser-to-CLI terminal streaming
2. **Zero server visibility** - Rails never sees keys or terminal data
3. **Per-user isolation** - Each user's tailnet is completely separate
4. **No MITM possible** - Key exchange via URL fragment (out-of-band)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              User's Tailnet                                  │
│                                                                              │
│    CLI                                                         Browser      │
│   ┌─────────────────┐                                    ┌─────────────────┐│
│   │ tailscaled      │◄══════ WireGuard Tunnel ═════════►│ tsconnect WASM  ││
│   │ + SSH server    │        (P2P or via DERP)          │ + xterm.js      ││
│   │ + Agent PTYs    │                                    │                 ││
│   └─────────────────┘                                    └─────────────────┘│
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
                                     │
                    ┌────────────────┴────────────────┐
                    │         Headscale Server        │
                    │  - Coordination (peer routing)  │
                    │  - DERP relay (fallback only)   │
                    │  - Cannot decrypt traffic       │
                    └─────────────────────────────────┘

Rails Server: Only serves static pages. Never sees keys or terminal data.
```

---

## Zero-Knowledge Key Exchange

```
1. CLI creates ephemeral pre-auth key (talks directly to Headscale)
2. CLI generates URL: https://botster.com/hub/abc#key=hskey_xxxxx
                                                  └── Fragment never sent to server
3. User scans QR code or clicks link
4. Browser extracts key from window.location.hash
5. Browser joins tailnet using key
6. WireGuard tunnel established - Rails never involved
```

**Why this is secure:**
- URL fragment (`#`) is never sent to server (HTTP spec)
- Server logs, analytics, CDNs see nothing
- Key exchange is out-of-band (QR code = physical verification)
- Once in tailnet, WireGuard handles all encryption

---

## Per-User Isolation

Each user gets their own Headscale namespace (like a private VPN):

```
Headscale Server
├── Namespace: user-123 (Alice)
│   ├── alice-cli
│   ├── alice-browser-1
│   └── alice-browser-2
│
├── Namespace: user-456 (Bob)
│   ├── bob-cli
│   └── bob-browser-1
│
└── (No cross-namespace communication possible)
```

**Enforced at infrastructure level** - not application code.

---

## Phase 1: Delete Legacy Code

Remove everything related to the old Action Cable + Olm architecture:

### Rails (Delete)
```
app/channels/terminal_channel.rb        # Action Cable relay
app/channels/tunnel_channel.rb          # Tunnel channel
app/javascript/crypto/olm.js            # Olm WASM wrapper
app/javascript/controllers/connection_controller.js  # Olm + Action Cable
app/javascript/wasm/vodozemac_wasm.js   # WASM bindings
public/wasm/vodozemac_wasm*             # WASM files
config/cable.yml                        # If no longer needed
```

### CLI (Delete)
```
cli/src/relay/olm.rs                    # Olm implementation
cli/src/relay/connection.rs             # Action Cable WebSocket
cli/src/relay/persistence.rs            # Olm session storage
cli/src/relay/                          # Likely entire module
```

### Other (Delete)
```
vodozemac-wasm/                         # Entire WASM crate
app/javascript/crypto/                  # Entire directory (if only olm)
docs/signal-protocol-migration-paused.md  # Obsolete
```

### Dependencies to Remove
```
# cli/Cargo.toml - remove:
vodozemac
tokio-tungstenite (if only for Action Cable)
```

---

## Phase 2: Headscale Server

### 2.1 Kamal Deployment

Add to `config/deploy.yml`:

```yaml
accessories:
  headscale:
    image: headscale/headscale:0.23
    host: headscale.botster.dev
    port: "8080:8080"
    cmd: serve
    directories:
      - data:/var/lib/headscale
    files:
      - config/headscale.yaml:/etc/headscale/config.yaml
    env:
      clear:
        HEADSCALE_SERVER_URL: https://headscale.botster.dev
```

### 2.2 Headscale Configuration

Create `config/headscale.yaml`:

```yaml
server_url: https://headscale.botster.dev
listen_addr: 0.0.0.0:8080

database:
  type: sqlite
  sqlite:
    path: /var/lib/headscale/db.sqlite

# Embedded DERP relay
derp:
  server:
    enabled: true
    region_id: 999
    region_code: botster
    stun_listen_addr: 0.0.0.0:3478

# IP allocation
prefixes:
  v4: 100.64.0.0/10
  v6: fd7a:115c:a1e0::/48

# Magic DNS
dns:
  magic_dns: true
  base_domain: tail.botster.dev
```

### 2.3 Admin API Key

After deployment, create admin API key:

```bash
kamal accessory exec headscale "headscale apikeys create --expiration 365d"
```

Store in Rails credentials as `headscale.admin_api_key`.

---

## Phase 3: Rails Integration (Minimal)

Rails' only role: manage user namespaces via Headscale API.

### 3.1 Headscale Client

```ruby
# app/services/headscale_client.rb
class HeadscaleClient
  include HTTParty
  base_uri ENV.fetch('HEADSCALE_URL', 'https://headscale.botster.dev')

  class << self
    def create_user(name)
      post('/api/v1/user', body: { name: name }.to_json, headers: headers)
    end

    def create_preauth_key(user:, ephemeral: false, expiration: 1.hour.from_now, tags: [])
      post('/api/v1/preauthkey', {
        body: {
          user: user,
          reusable: false,
          ephemeral: ephemeral,
          expiration: expiration.iso8601,
          aclTags: tags
        }.to_json,
        headers: headers
      }).parsed_response.dig('preAuthKey', 'key')
    end

    private

    def headers
      {
        'Authorization' => "Bearer #{Rails.application.credentials.headscale[:admin_api_key]}",
        'Content-Type' => 'application/json'
      }
    end
  end
end
```

### 3.2 User Namespace Creation

```ruby
# app/models/user.rb
class User < ApplicationRecord
  after_create :create_headscale_namespace

  def headscale_namespace
    "user-#{id}"
  end

  private

  def create_headscale_namespace
    HeadscaleClient.create_user(headscale_namespace)
  rescue => e
    Rails.logger.error "Failed to create Headscale namespace: #{e.message}"
  end
end
```

### 3.3 Hub Pre-Auth Key (for CLI)

```ruby
# app/models/hub.rb
class Hub < ApplicationRecord
  belongs_to :user

  # CLI's pre-auth key - stored encrypted, provided during registration
  encrypts :tailscale_preauth_key

  before_create :generate_tailscale_preauth_key

  private

  def generate_tailscale_preauth_key
    self.tailscale_preauth_key = HeadscaleClient.create_preauth_key(
      user: user.headscale_namespace,
      ephemeral: false,  # CLI is persistent
      expiration: 1.year.from_now,
      tags: ['tag:cli']
    )
  end
end
```

### 3.4 API Endpoint for CLI to Get Browser Keys

The CLI needs to create ephemeral keys for browsers. Instead of CLI having admin access, CLI requests through Rails but **Rails returns a scoped token** (not the actual key):

```ruby
# app/controllers/api/hubs/tailscale_controller.rb
module Api
  module Hubs
    class TailscaleController < ApplicationController
      before_action :authenticate_hub!

      # CLI calls this to get a browser pre-auth key
      # Returns encrypted so Rails can't use it
      def browser_key
        key = HeadscaleClient.create_preauth_key(
          user: current_hub.user.headscale_namespace,
          ephemeral: true,
          expiration: 1.hour.from_now,
          tags: ['tag:browser']
        )

        # Encrypt with CLI's public key so Rails can't read it
        encrypted = encrypt_for_cli(key, current_hub.cli_public_key)

        render json: { encrypted_key: encrypted }
      end

      private

      def encrypt_for_cli(plaintext, public_key)
        # Use NaCl box or similar
        # CLI has private key, decrypts locally
        RbNaCl::Box.new(public_key, server_private_key).encrypt(plaintext)
      end
    end
  end
end
```

**Alternative (simpler):** CLI stores a Headscale API key scoped to its user's namespace. Headscale doesn't support this natively, so we'd need the Rails proxy approach above.

---

## Phase 4: CLI Integration

### 4.1 New Dependencies

```toml
# cli/Cargo.toml
[dependencies]
# Remove vodozemac, tokio-tungstenite (Action Cable)

# Add (for shelling out to tailscale)
# No new deps needed if shelling out
```

### 4.2 Tailscale Module

```rust
// cli/src/tailscale.rs

use anyhow::Result;
use std::process::Command;

pub struct TailscaleClient {
    headscale_url: String,
}

impl TailscaleClient {
    pub fn new(headscale_url: &str) -> Self {
        Self {
            headscale_url: headscale_url.to_string(),
        }
    }

    /// Join the tailnet using a pre-auth key
    pub fn up(&self, preauth_key: &str) -> Result<()> {
        let status = Command::new("tailscale")
            .args([
                "up",
                "--login-server", &self.headscale_url,
                "--authkey", preauth_key,
                "--ssh",  // Enable Tailscale SSH
            ])
            .status()?;

        if !status.success() {
            anyhow::bail!("Failed to join tailnet");
        }
        Ok(())
    }

    /// Get this node's tailnet hostname
    pub fn hostname(&self) -> Result<String> {
        let output = Command::new("tailscale")
            .args(["status", "--json"])
            .output()?;

        let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(status["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string())
    }

    /// Check if connected to tailnet
    pub fn is_connected(&self) -> bool {
        Command::new("tailscale")
            .args(["status"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
```

### 4.3 Browser Key Generation

```rust
// cli/src/browser_connect.rs

use crate::api_client::ApiClient;
use crate::config::Config;
use anyhow::Result;

pub struct BrowserConnector {
    api_client: ApiClient,
    headscale_url: String,
}

impl BrowserConnector {
    /// Generate a URL for browser to connect
    /// Key is in fragment - server never sees it
    pub async fn generate_connect_url(&self) -> Result<String> {
        // Request browser key from Rails (encrypted)
        let encrypted_key = self.api_client.get_browser_preauth_key().await?;

        // Decrypt locally
        let preauth_key = self.decrypt_key(&encrypted_key)?;

        // Build URL with key in fragment
        let hub_url = format!("{}/hub/{}", self.api_client.base_url(), self.hub_identifier);
        let connect_url = format!("{}#key={}", hub_url, preauth_key);

        Ok(connect_url)
    }

    /// Generate QR code for the connect URL
    pub fn generate_qr_code(&self, url: &str) -> Result<String> {
        // Use qrcode crate to generate ASCII QR
        use qrcode::{QrCode, render::unicode};

        let code = QrCode::new(url)?;
        let image = code.render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build();

        Ok(image)
    }
}
```

### 4.4 Hub Startup Flow

```rust
// cli/src/hub/mod.rs

impl Hub {
    pub async fn start(&mut self) -> Result<()> {
        // 1. Join tailnet if not already connected
        if !self.tailscale.is_connected() {
            let preauth_key = self.config.tailscale_preauth_key()?;
            self.tailscale.up(&preauth_key)?;
        }

        // 2. Get our tailnet hostname for display
        let hostname = self.tailscale.hostname()?;
        info!("Connected to tailnet as {}", hostname);

        // 3. Display QR code for browser connection
        let connect_url = self.browser_connector.generate_connect_url().await?;
        let qr = self.browser_connector.generate_qr_code(&connect_url)?;
        println!("{}", qr);
        println!("Scan to connect: {}", connect_url);

        // 4. Start agent management loop (existing code)
        self.run_agent_loop().await
    }
}
```

### 4.5 Agent PTY via tmux

```rust
// cli/src/agent.rs

impl Agent {
    /// Start agent in a tmux session
    pub fn start(&mut self) -> Result<()> {
        let session_name = self.tmux_session_name();

        Command::new("tmux")
            .args([
                "new-session",
                "-d",  // Detached
                "-s", &session_name,
                "-c", &self.worktree_path,
                "claude", "--dangerously-skip-permissions",
            ])
            .status()?;

        self.tmux_session = Some(session_name);
        Ok(())
    }

    pub fn tmux_session_name(&self) -> String {
        format!("agent-{}-{}",
            self.repo.replace('/', "-"),
            self.issue_number
        )
    }
}
```

---

## Phase 5: Browser Integration

### 5.1 Get tsconnect WASM

Option A: Build from Tailscale source
```bash
git clone https://github.com/tailscale/tailscale
cd tailscale/cmd/tsconnect
./build.sh
cp build/* /path/to/rails/public/wasm/
```

Option B: Extract from headscale-console (they've done the work)
```bash
# Check their releases/packages
```

### 5.2 New Browser Controller

```javascript
// app/javascript/controllers/tailscale_controller.js
import { Controller } from "@hotwired/stimulus"
import { Terminal } from "xterm"
import { FitAddon } from "xterm-addon-fit"

export default class extends Controller {
  static targets = ["terminal", "status", "qrPrompt"]
  static values = {
    headscaleUrl: String,
  }

  connect() {
    this.initTerminal()
    this.checkForKey()
  }

  initTerminal() {
    this.term = new Terminal({ cursorBlink: true })
    this.fitAddon = new FitAddon()
    this.term.loadAddon(this.fitAddon)
    this.term.open(this.terminalTarget)
    this.fitAddon.fit()
  }

  checkForKey() {
    // Extract pre-auth key from URL fragment
    const hash = window.location.hash
    if (!hash || !hash.includes('key=')) {
      this.showQrPrompt()
      return
    }

    const params = new URLSearchParams(hash.substring(1))
    const preAuthKey = params.get('key')

    if (preAuthKey) {
      // Clear fragment from URL (don't leave key visible)
      history.replaceState(null, '', window.location.pathname)
      this.connectToTailnet(preAuthKey)
    } else {
      this.showQrPrompt()
    }
  }

  showQrPrompt() {
    this.qrPromptTarget.classList.remove('hidden')
    this.statusTarget.textContent = 'Scan QR code from CLI to connect'
  }

  async connectToTailnet(preAuthKey) {
    this.statusTarget.textContent = 'Loading Tailscale...'

    try {
      // Load tsconnect WASM
      await this.loadTsconnect()

      this.statusTarget.textContent = 'Joining tailnet...'

      // Initialize Tailscale client
      this.ipn = newIPN({
        controlURL: this.headscaleUrlValue,
        authKey: preAuthKey,
        hostname: `browser-${Date.now()}`,
      })

      // Run with callbacks
      this.ipn.run({
        notifyState: (state) => this.handleStateChange(state),
        notifyNetMap: (netMap) => this.handleNetMap(netMap),
        notifyBrowseToURL: (url) => console.log('Auth URL:', url),
      })
    } catch (error) {
      console.error('Failed to connect:', error)
      this.statusTarget.textContent = `Error: ${error.message}`
    }
  }

  async loadTsconnect() {
    // Load Go WASM support
    if (!window.Go) {
      await this.loadScript('/wasm/wasm_exec.js')
    }

    const go = new Go()
    const result = await WebAssembly.instantiateStreaming(
      fetch('/wasm/tsconnect.wasm'),
      go.importObject
    )
    go.run(result.instance)
  }

  loadScript(src) {
    return new Promise((resolve, reject) => {
      const script = document.createElement('script')
      script.src = src
      script.onload = resolve
      script.onerror = reject
      document.head.appendChild(script)
    })
  }

  handleStateChange(state) {
    console.log('Tailscale state:', state)
    this.statusTarget.textContent = `Tailscale: ${state}`

    if (state === 'Running') {
      this.statusTarget.textContent = 'Connected! Finding CLI...'
      // Wait a moment for peer discovery, then connect
      setTimeout(() => this.connectToCli(), 1000)
    }
  }

  handleNetMap(netMap) {
    console.log('Network map:', netMap)
    // Find CLI peer
    this.cliPeer = netMap.peers?.find(p => p.name?.includes('cli'))
  }

  connectToCli() {
    if (!this.cliPeer) {
      this.statusTarget.textContent = 'CLI not found in tailnet'
      return
    }

    const cliHostname = this.cliPeer.name
    this.statusTarget.textContent = `Connecting to ${cliHostname}...`

    // SSH to CLI
    this.sshSession = this.ipn.ssh(cliHostname, 'root', {
      writeFn: (data) => this.term.write(data),
      setReadFn: (fn) => { this.sshReadFn = fn },
      rows: this.term.rows,
      cols: this.term.cols,
      onConnected: () => {
        this.statusTarget.textContent = `Connected to ${cliHostname}`
        this.qrPromptTarget?.classList.add('hidden')
      },
      onDone: () => {
        this.statusTarget.textContent = 'Disconnected'
      },
    })

    // Send terminal input to SSH
    this.term.onData((data) => {
      this.sshReadFn?.(data)
    })

    // Handle resize
    this.term.onResize(({ rows, cols }) => {
      this.sshSession?.resize(rows, cols)
    })
  }

  disconnect() {
    this.sshSession?.close()
    this.term?.dispose()
  }
}
```

### 5.3 View Template

```erb
<%# app/views/hubs/show.html.erb %>
<div data-controller="tailscale"
     data-tailscale-headscale-url-value="<%= ENV['HEADSCALE_URL'] %>">

  <%# Status bar %>
  <div class="p-2 bg-gray-800 text-gray-300 text-sm">
    <span data-tailscale-target="status">Initializing...</span>
  </div>

  <%# QR code prompt (shown when no key in URL) %>
  <div data-tailscale-target="qrPrompt" class="hidden p-8 text-center">
    <p class="text-lg mb-4">Scan QR code from your CLI to connect</p>
    <p class="text-gray-500">The QR code contains a secure key that never touches our servers</p>
  </div>

  <%# Terminal %>
  <div data-tailscale-target="terminal" class="h-[600px] w-full bg-black"></div>
</div>
```

---

## Phase 6: Security Configuration

### 6.1 Headscale ACLs

```json
{
  "acls": [
    {
      "action": "accept",
      "src": ["tag:browser"],
      "dst": ["tag:cli:*"]
    },
    {
      "action": "accept",
      "src": ["tag:cli"],
      "dst": ["tag:browser:*"]
    }
  ],
  "ssh": [
    {
      "action": "accept",
      "src": ["tag:browser"],
      "dst": ["tag:cli"],
      "users": ["autogroup:nonroot"]
    }
  ],
  "tagOwners": {
    "tag:cli": ["autogroup:admin"],
    "tag:browser": ["autogroup:admin"]
  }
}
```

---

## File Summary

### Delete
```
app/channels/terminal_channel.rb
app/channels/tunnel_channel.rb
app/javascript/crypto/olm.js
app/javascript/controllers/connection_controller.js
app/javascript/wasm/vodozemac_wasm.js
public/wasm/vodozemac_wasm*
vodozemac-wasm/                          (entire directory)
cli/src/relay/olm.rs
cli/src/relay/connection.rs
cli/src/relay/persistence.rs
cli/src/relay/                           (likely entire module)
```

### Create
```
config/headscale.yaml                    (Headscale config)
app/services/headscale_client.rb         (API client)
app/javascript/controllers/tailscale_controller.js  (Browser Tailscale)
cli/src/tailscale.rs                     (Tailscale wrapper)
cli/src/browser_connect.rs               (QR code / URL generation)
public/wasm/tsconnect.wasm               (Tailscale WASM)
public/wasm/wasm_exec.js                 (Go WASM support)
```

### Modify
```
config/deploy.yml                        (Add Headscale accessory)
app/models/user.rb                       (Create namespace on signup)
app/models/hub.rb                        (Store Tailscale pre-auth key)
cli/Cargo.toml                           (Remove old deps)
cli/src/hub/mod.rs                       (Tailscale integration)
cli/src/agent.rs                         (tmux session management)
app/views/hubs/show.html.erb             (New terminal view)
```

---

## Implementation Order

1. **Delete legacy code** - Clean slate
2. **Deploy Headscale** - Infrastructure first
3. **CLI Tailscale integration** - Join tailnet, display QR
4. **Browser tsconnect** - Load WASM, join tailnet, SSH
5. **Test E2E** - Full flow verification
6. **Polish** - Error handling, reconnection, UX

---

## Verification Checklist

- [ ] Headscale server accessible
- [ ] User namespace created on signup
- [ ] CLI joins tailnet with pre-auth key
- [ ] CLI displays QR code with key in fragment
- [ ] Browser extracts key from fragment (Rails logs show no key)
- [ ] Browser joins tailnet
- [ ] Browser SSHs to CLI
- [ ] Terminal I/O works
- [ ] Multiple browsers can connect
- [ ] Different users' tailnets are isolated
- [ ] DERP relay works when P2P fails
