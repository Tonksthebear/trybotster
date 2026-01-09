# Headscale Implementation Guide

**Status**: Planning
**Goal**: Browser connects to CLI agents via Headscale/WireGuard + SSH

---

## Architecture Overview

**Key principle**: Each user has their own isolated tailnet (Headscale "user"/namespace). A user's CLI and browsers can only see each other - complete isolation from other users.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                            User A's Tailnet                                  │
│                                                                              │
│   Browser ◄────── WireGuard (P2P or DERP) ──────► CLI                       │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                            User B's Tailnet                                  │
│                                                                              │
│   Browser ◄────── WireGuard (P2P or DERP) ──────► CLI                       │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘

                    No cross-user visibility possible

┌─────────────────────────────────────────────────────────────────────────────┐
│                         Headscale Server                                     │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  Coordination (manages all users' tailnets, but isolates them)       │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  DERP Relay (forwards encrypted packets, can't read them)            │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Per-User Isolation

| User | Headscale Namespace | Nodes |
|------|---------------------|-------|
| alice | `user-alice-123` | alice's CLI, alice's browsers |
| bob | `user-bob-456` | bob's CLI, bob's browsers |

Alice's devices can never see or connect to Bob's devices.

---

## Phase 1: Headscale Server

### 1.1 Kamal Deployment

Add Headscale as an accessory in `config/deploy.yml`:

```yaml
accessories:
  headscale:
    image: headscale/headscale:0.23
    host: headscale.botster.dev  # or same host, different port
    port: 8080:8080
    volumes:
      - headscale_data:/var/lib/headscale
      - headscale_config:/etc/headscale
    cmd: serve
    env:
      clear:
        HEADSCALE_SERVER_URL: https://headscale.botster.dev
        HEADSCALE_LISTEN_ADDR: 0.0.0.0:8080
```

### 1.2 Headscale Configuration

Create `config/headscale/config.yaml`:

```yaml
server_url: https://headscale.botster.dev
listen_addr: 0.0.0.0:8080
metrics_listen_addr: 127.0.0.1:9090

# Database (SQLite is fine for our scale)
database:
  type: sqlite
  sqlite:
    path: /var/lib/headscale/db.sqlite

# DERP - embedded relay server
derp:
  server:
    enabled: true
    region_id: 999
    region_code: botster
    region_name: Botster
    stun_listen_addr: 0.0.0.0:3478
  urls: []  # Don't use external DERP
  auto_update_enabled: false

# Prefixes for the tailnet
prefixes:
  v4: 100.64.0.0/10
  v6: fd7a:115c:a1e0::/48

# DNS (optional but useful)
dns:
  magic_dns: true
  base_domain: botster.internal

# Disable default Tailscale DERP regions (use only our embedded one)
derp:
  paths: []
```

### 1.3 User Namespace Creation

Each Botster user gets their own Headscale namespace (created automatically via API when user signs up or creates first hub):

```ruby
# app/models/user.rb
class User < ApplicationRecord
  after_create :create_headscale_namespace

  def headscale_namespace
    "user-#{id}"
  end

  private

  def create_headscale_namespace
    HeadscaleClient.create_namespace(headscale_namespace)
  end
end
```

### 1.4 API Key for Rails

Generate an API key for Rails to create pre-auth keys:

```bash
kamal accessory exec headscale "headscale apikeys create --expiration 365d"
```

Store in Rails credentials:

```yaml
headscale:
  url: https://headscale.botster.dev
  api_key: <generated-key>
```

---

## Phase 2: CLI Integration

### 2.1 Tailscale on CLI Host

The CLI needs to join the tailnet. Options:

**Option A: System tailscaled (recommended for simplicity)**
```bash
# Install tailscale on the host
curl -fsSL https://tailscale.com/install.sh | sh

# Join headscale with pre-auth key
tailscale up --login-server=https://headscale.botster.dev --authkey=<preauth-key>

# Enable SSH
tailscale set --ssh
```

**Option B: Containerized tailscaled**
```yaml
# docker-compose.yml alongside CLI
services:
  tailscale:
    image: tailscale/tailscale:latest
    cap_add:
      - NET_ADMIN
    volumes:
      - /dev/net/tun:/dev/net/tun
      - tailscale_state:/var/lib/tailscale
    environment:
      TS_AUTHKEY: ${TAILSCALE_AUTHKEY}
      TS_EXTRA_ARGS: --login-server=https://headscale.botster.dev --ssh
```

### 2.2 Pre-Auth Key for CLI

During hub registration, Rails provides a pre-auth key scoped to the user's namespace:

```ruby
# app/models/hub.rb
class Hub < ApplicationRecord
  belongs_to :user

  after_create :create_tailscale_preauth_key

  private

  def create_tailscale_preauth_key
    key = HeadscaleClient.create_preauth_key(
      namespace: user.headscale_namespace,  # User's isolated tailnet
      reusable: false,
      ephemeral: false,
      expiration: 1.year.from_now,
      tags: ["tag:cli", "tag:hub-#{id}"]
    )
    update!(tailscale_preauth_key: key)
  end
end
```

The CLI joins the **user's tailnet**, not a shared one.

CLI uses this during setup:

```rust
// cli/src/hub/registration.rs
pub async fn setup_tailscale(preauth_key: &str, headscale_url: &str) -> Result<()> {
    let status = Command::new("tailscale")
        .args(["up",
               &format!("--login-server={}", headscale_url),
               &format!("--authkey={}", preauth_key)])
        .status()?;

    if !status.success() {
        return Err(anyhow!("Failed to join tailnet"));
    }

    // Enable SSH
    Command::new("tailscale")
        .args(["set", "--ssh"])
        .status()?;

    Ok(())
}
```

### 2.3 Agent Sessions with tmux

Each agent runs in a named tmux session:

```rust
// cli/src/agent.rs
impl Agent {
    pub fn start_in_tmux(&self) -> Result<()> {
        let session_name = format!("agent-{}-{}", self.repo.replace('/', "-"), self.issue);

        Command::new("tmux")
            .args([
                "new-session", "-d",
                "-s", &session_name,
                "-c", &self.worktree_path,
                "claude", "--agent"  // or whatever the agent command is
            ])
            .status()?;

        Ok(())
    }

    pub fn session_name(&self) -> String {
        format!("agent-{}-{}", self.repo.replace('/', "-"), self.issue)
    }
}
```

---

## Phase 3: Browser Integration

### 3.1 tsconnect WASM Setup

Download/build tsconnect WASM:

```bash
# Clone tailscale
git clone https://github.com/tailscale/tailscale.git
cd tailscale/cmd/tsconnect

# Build WASM
./tool/go run ./cmd/tsconnect build

# Copy artifacts to Rails
cp build/wasm_exec.js ../../app/javascript/wasm/
cp build/main.wasm ../../public/wasm/tsconnect.wasm
```

Or use pre-built from headscale-console as reference.

### 3.2 Rails: Ephemeral Auth Keys for Browsers

```ruby
# app/services/headscale_client.rb
class HeadscaleClient
  include HTTParty
  base_uri Rails.application.credentials.headscale[:url]

  def self.create_namespace(namespace)
    post('/api/v1/namespace', {
      headers: auth_headers,
      body: { name: namespace }.to_json
    })
  end

  def self.create_preauth_key(namespace:, reusable: false, ephemeral: false, expiration:, tags: [])
    response = post('/api/v1/preauthkey', {
      headers: auth_headers,
      body: {
        namespace: namespace,
        reusable: reusable,
        ephemeral: ephemeral,
        expiration: expiration.iso8601,
        aclTags: tags
      }.to_json
    })
    response.parsed_response['preAuthKey']
  end

  def self.create_ephemeral_key(user:)
    create_preauth_key(
      namespace: user.headscale_namespace,  # User's isolated tailnet
      reusable: false,
      ephemeral: true,  # Auto-cleanup when browser disconnects
      expiration: 1.hour.from_now,
      tags: ["tag:browser"]
    )
  end

  private

  def self.auth_headers
    { 'Authorization' => "Bearer #{api_key}", 'Content-Type' => 'application/json' }
  end

  def self.api_key
    Rails.application.credentials.headscale[:api_key]
  end
end

# app/controllers/api/tailscale_controller.rb
module Api
  class TailscaleController < ApplicationController
    def auth_key
      key = HeadscaleClient.create_ephemeral_key(user: current_user)
      render json: { auth_key: key }
    end
  end
end
```

### 3.3 Browser JavaScript

```javascript
// app/javascript/controllers/tailscale_terminal_controller.js
import { Controller } from "@hotwired/stimulus"
import { Terminal } from "xterm"
import { FitAddon } from "xterm-addon-fit"

export default class extends Controller {
  static targets = ["terminal"]
  static values = {
    headscaleUrl: String,
    cliHostname: String,
    agentSession: String
  }

  async connect() {
    // Initialize xterm
    this.term = new Terminal({ cursorBlink: true })
    this.fitAddon = new FitAddon()
    this.term.loadAddon(this.fitAddon)
    this.term.open(this.terminalTarget)
    this.fitAddon.fit()

    // Get ephemeral auth key from Rails
    const response = await fetch('/api/tailscale/auth_key')
    const { auth_key } = await response.json()

    // Load tsconnect WASM
    await this.initTailscale(auth_key)
  }

  async initTailscale(authKey) {
    // Load WASM
    const go = new Go()
    const result = await WebAssembly.instantiateStreaming(
      fetch('/wasm/tsconnect.wasm'),
      go.importObject
    )
    go.run(result.instance)

    // Initialize IPN (Tailscale client)
    this.ipn = newIPN({
      controlURL: this.headscaleUrlValue,
      authKey: authKey,
      hostname: `browser-${Date.now()}`
    })

    // Start and wait for connection
    this.ipn.run({
      notifyState: (state) => this.handleStateChange(state),
      notifyNetMap: (netMap) => this.handleNetMap(netMap),
      notifyBrowseToURL: (url) => console.log('Auth URL:', url)
    })
  }

  handleStateChange(state) {
    console.log('Tailscale state:', state)
    if (state === 'Running') {
      this.connectSSH()
    }
  }

  handleNetMap(netMap) {
    console.log('Peers:', netMap.peers)
  }

  connectSSH() {
    // SSH to CLI and attach to tmux session
    const command = this.agentSessionValue
      ? `tmux attach-session -t ${this.agentSessionValue}`
      : 'tmux list-sessions'

    this.sshSession = this.ipn.ssh(this.cliHostnameValue, 'botster', {
      writeFn: (data) => this.term.write(data),
      setReadFn: (fn) => this.term.onData(fn),
      rows: this.term.rows,
      cols: this.term.cols,
      onConnected: () => {
        console.log('SSH connected')
        // Send command to attach to tmux session
        // (or configure SSH to run this automatically)
      },
      onDone: () => console.log('SSH disconnected')
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

### 3.4 Rails View

```erb
<%# app/views/hubs/show.html.erb %>
<div data-controller="tailscale-terminal"
     data-tailscale-terminal-headscale-url-value="<%= Rails.application.credentials.headscale[:url] %>"
     data-tailscale-terminal-cli-hostname-value="<%= @hub.tailscale_hostname %>"
     data-tailscale-terminal-agent-session-value="<%= @agent&.tmux_session_name %>">

  <div data-tailscale-terminal-target="terminal"
       class="h-[600px] w-full bg-black rounded-lg"></div>
</div>
```

---

## Phase 4: SSH Configuration

### 4.1 Tailscale SSH (No Separate SSH User Needed)

Tailscale SSH handles authentication via the tailnet - no Unix users or SSH keys required. The browser is authenticated because it joined the user's tailnet with a valid pre-auth key.

```bash
# On CLI host, just enable Tailscale SSH
tailscale set --ssh
```

### 4.2 SSH ACLs Per Namespace

Each user's namespace has its own ACL policy. Since nodes in different namespaces can't see each other anyway, the ACL just controls what browsers can do within their own tailnet:

```json
{
  "acls": [
    {
      "action": "accept",
      "src": ["tag:browser"],
      "dst": ["tag:cli:*"]
    }
  ],
  "ssh": [
    {
      "action": "accept",
      "src": ["tag:browser"],
      "dst": ["tag:cli"],
      "users": ["autogroup:nonroot"]
    }
  ]
}
```

### 4.3 SSH Command to Attach to tmux

When the browser SSHs in, it should automatically attach to the agent session. Options:

**Option A: Pass session name in SSH command**
```javascript
// In browser JavaScript
this.ipn.ssh(cliHostname, 'root', {
  // ... terminal config
})
// Then send command after connection:
// `tmux attach-session -t agent-repo-issue-123`
```

**Option B: Custom shell script on CLI**
```bash
# /usr/local/bin/agent-shell
#!/bin/bash
SESSION="$1"
if [ -n "$SESSION" ]; then
    exec tmux attach-session -t "$SESSION"
else
    echo "Available sessions:"
    tmux list-sessions
fi
```

---

## Testing Checklist

### Phase 1: Headscale
- [ ] Headscale accessible at configured URL
- [ ] Can create API keys
- [ ] Can create pre-auth keys via API
- [ ] DERP relay accessible

### Phase 2: CLI
- [ ] CLI joins tailnet on startup
- [ ] CLI appears in `headscale nodes list`
- [ ] Tailscale SSH enabled
- [ ] Can SSH from another Tailscale node
- [ ] tmux sessions created for agents

### Phase 3: Browser
- [ ] tsconnect WASM loads
- [ ] Browser joins tailnet (ephemeral)
- [ ] Browser can SSH to CLI
- [ ] xterm.js displays terminal output
- [ ] Input flows back to CLI

### Phase 4: Full Flow
- [ ] User logs in to Rails
- [ ] User opens hub page
- [ ] Browser auto-connects to tailnet
- [ ] Browser SSHs to CLI
- [ ] User sees agent terminal
- [ ] User can interact with agent

---

## Security Model

### User Isolation (Most Important)

Each user has their own Headscale namespace = their own isolated network.

```
User A's namespace: user-123
  - alice-cli (her hub)
  - alice-browser-1
  - alice-browser-2

User B's namespace: user-456
  - bob-cli (his hub)
  - bob-browser-1

These cannot see or connect to each other. Enforced at the Headscale level.
```

### Additional Security Layers

1. **Per-user tailnets**: Complete network isolation between users
2. **Ephemeral browser keys**: Auto-expire (1 hour), auto-cleanup on disconnect
3. **ACLs**: Browsers can only SSH to CLI nodes, nothing else
4. **E2E encryption**: WireGuard encrypts all traffic, DERP relay can't decrypt
5. **No long-term browser secrets**: Auth key is short-lived, fetched fresh each session
6. **Headscale sees nothing**: Only manages coordination, never sees decrypted traffic
