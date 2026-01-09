# Headscale + SSH Browser Architecture

**Date**: 2025-01-08
**Status**: **ACTIVE** - Implementing Headscale SSH approach

## Executive Summary

Browser connects to CLI via **Headscale/DERP + SSH**:

1. Browser loads tsconnect WASM (full Tailscale client)
2. Browser joins Headscale tailnet via DERP WebSocket
3. Browser SSHs to CLI node using `ipn.ssh()` API
4. xterm.js renders terminal output

**Security**: End-to-end WireGuard encryption. DERP relay only sees encrypted packets.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              Browser                                     │
│  ┌─────────────┐     ┌──────────────────┐     ┌───────────────────┐    │
│  │  Rails UI   │────▶│  tsconnect WASM  │────▶│    xterm.js       │    │
│  │  (hub page) │     │  (Tailscale)     │     │  (SSH terminal)   │    │
│  └─────────────┘     └────────┬─────────┘     └───────────────────┘    │
│                               │                                         │
│                               │ WireGuard encrypted                     │
└───────────────────────────────┼─────────────────────────────────────────┘
                                │ WebSocket
                                ▼
┌───────────────────────────────────────────────────────────────────────┐
│                        Headscale Server                                │
│  ┌────────────────────┐     ┌────────────────────┐                    │
│  │  Coordination      │     │  DERP Relay        │                    │
│  │  (auth, peer info) │     │  (WebSocket ↔ UDP) │                    │
│  └────────────────────┘     └─────────┬──────────┘                    │
└───────────────────────────────────────┼───────────────────────────────┘
                                        │ WireGuard encrypted
                                        ▼
┌───────────────────────────────────────────────────────────────────────┐
│                           CLI (Hub)                                    │
│  ┌────────────────────┐     ┌────────────────────┐                    │
│  │  Tailscale Client  │     │  SSH Server        │                    │
│  │  (tailscaled)      │────▶│  (Tailscale SSH)   │                    │
│  └────────────────────┘     └─────────┬──────────┘                    │
│                                       │                                │
│                                       ▼                                │
│                          ┌────────────────────────┐                   │
│                          │  tmux sessions         │                   │
│                          │  - agent-repo-issue-1  │                   │
│                          │  - agent-repo-issue-2  │                   │
│                          └────────────────────────┘                   │
└───────────────────────────────────────────────────────────────────────┘
```

---

## Components

### 1. Headscale Server (Deploy)

Self-hosted Tailscale control plane. Runs coordination + embedded DERP.

```yaml
# docker-compose.yml
services:
  headscale:
    image: headscale/headscale:latest
    ports:
      - "8080:8080"   # HTTP (coordination)
      - "443:443"     # HTTPS
      - "3478:3478"   # STUN
    volumes:
      - ./headscale/config:/etc/headscale
      - ./headscale/data:/var/lib/headscale
    command: serve
```

Key config (`config.yaml`):
```yaml
server_url: https://headscale.yourdomain.com
listen_addr: 0.0.0.0:8080
derp:
  server:
    enabled: true
    region_id: 999
    stun_listen_addr: 0.0.0.0:3478
```

### 2. CLI as Tailscale Node

CLI runs Tailscale client to join the tailnet.

**Option A**: Use system tailscaled
```bash
# On CLI host
tailscale up --login-server https://headscale.yourdomain.com --authkey <preauth-key>
```

**Option B**: Embed in Rust CLI using `tailscale-rs` or shell out to `tailscale`

### 3. Tailscale SSH on CLI

Enable Tailscale's built-in SSH server (no OpenSSH needed):

```bash
tailscale set --ssh
```

Or in config, advertise SSH capability.

### 4. tsconnect in Browser

Load tsconnect WASM and connect via API:

```javascript
import initWasm, { newIPN } from './tsconnect/wasm_exec.js';

await initWasm();

const ipn = newIPN({
  controlURL: 'https://headscale.yourdomain.com',
  authKey: ephemeralKey,  // Get from Rails API
  hostname: `browser-${userId}-${Date.now()}`,
});

ipn.run({
  notifyState: (state) => console.log('State:', state),
  notifyNetMap: (netMap) => console.log('Peers:', netMap.peers),
});

// Once connected (state === 'Running'), SSH to CLI:
const session = ipn.ssh(cliHostname, 'user', {
  writeFn: (data) => xterm.write(data),
  setReadFn: (readFn) => xterm.onData(readFn),
  rows: xterm.rows,
  cols: xterm.cols,
  onConnected: () => console.log('SSH connected!'),
  onDone: () => console.log('SSH session ended'),
});

// Handle terminal resize
xterm.onResize(({ rows, cols }) => session.resize(rows, cols));
```

### 5. Agent Sessions via tmux

Each agent runs in a tmux session. Browser SSH attaches to the session:

```bash
# Agent starts in tmux
tmux new-session -d -s "agent-${repo}-${issue}" "claude --agent"

# Browser SSH attaches
tmux attach-session -t "agent-${repo}-${issue}"
```

Or use a wrapper script on the CLI that the SSH user runs automatically.

---

## Auth Flow

### Browser Gets Ephemeral Auth Key

1. User is logged in to Rails (GitHub OAuth)
2. Rails calls Headscale API to create ephemeral pre-auth key
3. Rails returns key to browser
4. Browser uses key with tsconnect

```ruby
# app/services/headscale_client.rb
class HeadscaleClient
  def create_ephemeral_key(user:)
    # POST /api/v1/preauthkey
    response = connection.post('/api/v1/preauthkey', {
      user: user.headscale_user,
      ephemeral: true,
      expiration: 1.hour.from_now.iso8601,
    })
    response.body['preAuthKey']
  end
end
```

### CLI Pre-Auth Key

CLI gets a longer-lived pre-auth key during hub registration:

```rust
// During hub registration
let preauth_key = api_client.create_cli_preauth_key(&hub_id)?;
tailscale::up(&preauth_key, &headscale_url)?;
```

---

## Implementation Phases

### Phase 1: Headscale Infrastructure
- [ ] Deploy Headscale server
- [ ] Configure DERP with WebSocket support
- [ ] Test with native Tailscale clients

### Phase 2: CLI Integration
- [ ] CLI joins tailnet on startup
- [ ] Enable Tailscale SSH
- [ ] Wrap agents in tmux sessions
- [ ] Test SSH from another Tailscale node

### Phase 3: Browser Integration
- [ ] Build/integrate tsconnect WASM
- [ ] Rails API for ephemeral auth keys
- [ ] Browser connects to tailnet
- [ ] Browser SSHs to CLI
- [ ] xterm.js integration

### Phase 4: UX Polish
- [ ] Agent session picker UI
- [ ] Reconnection handling
- [ ] Multiple browser sessions
- [ ] Session persistence

---

## Encryption Comparison

| Approach | Protocol | Forward Secrecy | Relay Visibility |
|----------|----------|-----------------|------------------|
| **Headscale** | WireGuard Noise | Per-session | DERP sees encrypted blobs |
| libsignal | X3DH + Double Ratchet | Per-message | Rails sees encrypted blobs |
| Olm (current) | vodozemac Olm | Per-session | Rails sees encrypted blobs |

All secure. Headscale uses battle-tested WireGuard. DERP cannot decrypt traffic.

---

## Why NOT libsignal

We explored this approach (stashed in `signal-protocol-libsignal-migration`). Issues:

1. **AGPL license**: Requires open-sourcing the full application
2. **API instability**: libsignal-protocol Rust crate changed significantly between versions
3. **Complex integration**: Hit multiple API mismatches during WASM compilation
4. **Still needs relay**: Rails Action Cable still routes encrypted blobs
5. **Overkill for streaming**: Signal Protocol designed for async messaging

See `docs/signal-protocol-migration-paused.md` for full details.

---

## Headscale/tsconnect Research

## Existing Solutions

### 1. headscale-console
https://github.com/rickli-cloud/headscale-console

- Browser-based SSH, VNC, RDP to Headscale nodes
- Uses tsconnect WASM under the hood
- Svelte frontend
- Stateless (no database)
- Can self-host alongside Headscale

**Pros**: Already built, feature-rich, actively maintained
**Cons**: May have more than we need, different tech stack (Svelte)

### 2. Tailscale SSH Console
https://tailscale.com/kb/1216/tailscale-ssh-console

- Tailscale's official browser SSH client
- Same architecture (tsconnect WASM + DERP)
- Requires Tailscale account (not Headscale)

### 3. WebVM + Tailscale
https://labs.leaningtech.com/blog/webvm-virtual-machine-with-networking-via-tailscale

- Full Linux VM in browser with Tailscale networking
- Proves the concept works for complex use cases

## Options for Botster

### Option A: Adapt headscale-console

Fork or use headscale-console as the browser frontend.

**Architecture:**
```
Headscale Server (coordination + DERP)
        ↑
        │ WebSocket (DERP) + HTTPS (coordination)
        ↓
┌───────────────────┐
│  headscale-console│  ← Browser
│  (Svelte + WASM)  │
└───────────────────┘
        ↑
        │ WireGuard tunnel (via DERP)
        ↓
┌───────────────────┐
│   Botster CLI     │  ← Running agents
│  (Tailscale node) │
└───────────────────┘
```

**Pros:**
- Minimal work - already built
- SSH terminal already works
- Proven architecture

**Cons:**
- Different frontend stack (Svelte vs Rails/Hotwire)
- May need customization for agent management UI
- Another service to deploy

### Option B: Integrate tsconnect directly into Rails

Use tsconnect WASM in our existing Rails/Hotwire frontend.

**Architecture:**
```
Rails Server (UI + API)
        │
        │ HTTPS
        ↓
┌───────────────────┐
│  Rails Views +    │
│  tsconnect WASM   │  ← Browser
└───────────────────┘
        │
        │ WebSocket to Headscale DERP
        ↓
Headscale Server (DERP only?)
        │
        │ WireGuard tunnel
        ↓
┌───────────────────┐
│   Botster CLI     │
└───────────────────┘
```

**Pros:**
- Keep existing Rails stack
- More control over UX
- Single codebase

**Cons:**
- More integration work
- Need to understand tsconnect internals
- May fight the WASM build process

### Option C: Hybrid - Rails UI + headscale-console iframe

Keep Rails for agent management, embed headscale-console for terminal.

**Pros:**
- Best of both worlds
- Quick to implement

**Cons:**
- Two systems to maintain
- iframe security considerations

## Key Questions to Answer

1. **Does Botster already run Headscale?**
   - If yes, DERP is already available
   - If no, need to deploy Headscale

2. **What's the auth flow?**
   - How does browser get auth key to join tailnet?
   - Ephemeral keys? User login?

3. **Do we need the full headscale-console?**
   - Or just the terminal component?
   - Could extract just tsconnect + xterm.js

4. **What about the Rails Action Cable relay?**
   - Can we remove it entirely?
   - Or keep for non-terminal features?

## Comparison: Signal Protocol vs DERP

| Aspect | Signal Protocol | DERP/WireGuard |
|--------|-----------------|----------------|
| Crypto foundation | X3DH + Double Ratchet | Noise Protocol |
| Forward secrecy | Per-message | Per-session |
| Relay visibility | Encrypted blobs | Encrypted blobs |
| Audited | Yes (Signal) | Yes (WireGuard) |
| Implementation | Custom WASM (complex) | tsconnect (ready) |
| Trust signal | "We use Signal Protocol" | "We use WireGuard" |

Both provide E2E encryption. WireGuard is arguably more appropriate for real-time streaming (it's designed for VPN traffic), while Signal is designed for async messaging.

## Recommended Next Steps

1. **Verify Headscale setup** - Confirm DERP WebSocket support is enabled
2. **Test headscale-console** - Deploy locally and verify it works with your Headscale
3. **Evaluate integration approach** - Based on testing, choose Option A, B, or C
4. **Prototype terminal** - Get basic terminal working with new architecture
5. **Plan Action Cable removal** - If DERP replaces the relay

## References

- [Tailscale SSH Console Blog](https://tailscale.com/blog/ssh-console)
- [DERP Servers Documentation](https://tailscale.com/kb/1232/derp-servers)
- [headscale-console GitHub](https://github.com/rickli-cloud/headscale-console)
- [Headscale WebSocket DERP PR](https://github.com/juanfont/headscale/pull/2132)
- [tsconnect Source](https://github.com/tailscale/tailscale/tree/main/cmd/tsconnect)
