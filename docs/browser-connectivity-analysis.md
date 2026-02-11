# Browser-to-Terminal E2E Encryption Analysis

**Date**: 2025-01-08
**Goal**: Trustworthy connection from browser to terminal. Server stores/sees nothing.

---

## The Problem

Users view and interact with running agents in their browser. The terminal output may contain:
- Source code
- API keys, secrets
- Private business logic
- Personal data

**Requirements:**
1. End-to-end encryption between browser and CLI
2. Rails server cannot read terminal content
3. Users can trust the system (verifiable security)
4. Business protection (minimize liability for data handling)

---

## First Principles Analysis

### What does "server sees nothing" actually mean?

| Level | What Server Sees | Trust Model |
|-------|------------------|-------------|
| **Level 1**: Plaintext relay | Everything | "Trust us" |
| **Level 2**: Encrypted relay | Encrypted blobs | "We can't read it" |
| **Level 3**: No relay | Nothing (P2P) | "We don't touch it" |

**Level 3 is the gold standard** for user trust and liability protection.

---

## Options

### Option A: WebRTC P2P (Level 3)

```
Browser ←──── WebRTC Data Channel ────→ CLI
                    ↑
              (DTLS encrypted)

Rails only handles signaling (SDP exchange).
No terminal data ever touches Rails.
```

**How it works:**
1. Browser creates WebRTC offer (SDP)
2. Rails relays offer to CLI (via message queue)
3. CLI creates answer, Rails relays back
4. Browser and CLI establish direct connection
5. Terminal data flows directly, encrypted with DTLS

**Crypto:** DTLS 1.2/1.3 (based on TLS, mandatory in WebRTC)

**Pros:**
- Server literally never sees terminal data
- Strongest liability position
- Well-understood technology
- Pure Rust implementation available (`webrtc = "0.11"`)
- Design already documented (`docs/webrtc-p2p-design.md`)

**Cons:**
- NAT traversal can fail (symmetric NAT, corporate firewalls)
- Requires TURN relay fallback for reliability

**NAT Failure Mitigation:**
- Use TURN relay (self-hosted or service)
- TURN sees DTLS packets it can't decrypt
- Can be separate service from Rails (isolate liability)

---

### Option B: WireGuard via Headscale/DERP (Level 2)

```
Browser (tsconnect WASM) ──→ DERP Relay ──→ CLI (Tailscale)
           ↑                      ↑               ↑
      WireGuard              Encrypted        WireGuard
                              blobs
```

**How it works:**
1. Deploy Headscale server (coordination + DERP)
2. CLI joins tailnet via Tailscale client
3. Browser loads tsconnect WASM, joins tailnet
4. Browser SSHs to CLI over WireGuard tunnel
5. DERP relays encrypted packets

**Crypto:** WireGuard Noise Protocol (audited, battle-tested)

**Pros:**
- WireGuard is highly trusted protocol
- DERP cannot decrypt traffic
- Built-in SSH support via tsconnect
- Always works (DERP relay fallback)

**Cons:**
- Requires Headscale server infrastructure
- Browser WASM is large (full Tailscale client + gVisor)
- Rails still involved (auth key generation)
- Data touches relay server (even if encrypted)
- SSH model requires tmux for agent session management

---

### Option C: Signal Protocol via Action Cable (Level 2)

```
Browser ──→ Rails (Action Cable) ──→ CLI
    ↑             ↑                    ↑
Signal          Encrypted          Signal
Protocol        blobs              Protocol
```

**How it works:**
1. CLI publishes PreKeyBundle (X3DH parameters)
2. Browser establishes Signal session
3. All messages encrypted with Double Ratchet
4. Rails relays encrypted blobs via WebSocket

**Crypto:** X3DH + Double Ratchet (per-message forward secrecy)

**Pros:**
- Strongest cryptographic guarantees
- Per-message forward secrecy
- "We use Signal Protocol" trust signal

**Cons:**
- **AGPL license** (must open source if used)
- Complex implementation (we hit multiple API issues)
- Overkill for real-time streaming (designed for async messaging)
- Data still touches Rails (encrypted)
- Stashed due to complexity (`signal-protocol-libsignal-migration`)

---

### Option D: Noise Protocol over WebSocket (Level 2)

```
Browser ──→ Rails (Action Cable) ──→ CLI
    ↑             ↑                    ↑
  Noise        Encrypted            Noise
  Protocol     blobs                Protocol
```

**How it works:**
1. Implement Noise_XX handshake over WebSocket
2. Browser and CLI exchange public keys
3. All subsequent messages encrypted
4. Rails relays encrypted blobs

**Crypto:** Noise Protocol (same foundation as WireGuard)

**Pros:**
- Simpler than Signal (no ratchet)
- Simpler than Headscale (no infrastructure)
- Well-audited protocol
- MIT licensed implementations available

**Cons:**
- Custom implementation needed
- No per-message forward secrecy
- Data still touches Rails (encrypted)

---

## Comparison Matrix

| Criteria | WebRTC | Headscale | Signal | Noise |
|----------|--------|-----------|--------|-------|
| Data touches Rails | **No** | No (touches DERP) | Yes | Yes |
| Crypto strength | DTLS | WireGuard | X3DH+DR | Noise |
| Infrastructure | STUN/TURN | Headscale server | None | None |
| Implementation | Designed | Research | Paused | TBD |
| License risk | None | None | **AGPL** | None |
| Browser complexity | Low | High (big WASM) | Medium | Medium |
| Reliability | NAT issues | Always works | Always works | Always works |

---

## Decision: Headscale/WireGuard

**Chosen approach:** Headscale with tsconnect browser client.

**Rationale:**
1. Built on Tailscale - battle-tested, widely trusted
2. WireGuard encryption is well-known to security-conscious users
3. Open source clients available for reference
4. Easy deployment via Kamal
5. SSH works out of the box via tsconnect API
6. P2P when possible, DERP fallback guarantees connectivity

---

## Implementation Plan

See `docs/headscale-implementation.md` for detailed implementation guide.

---

## References

- [WebRTC Design Doc](./webrtc-p2p-design.md)
- [Signal Protocol Migration (Paused)](./signal-protocol-migration-paused.md)
- [Headscale Console](https://github.com/rickli-cloud/headscale-console)
- [tsconnect WASM API](https://pkg.go.dev/tailscale.com/cmd/tsconnect/wasm)
- [Noise Protocol](https://noiseprotocol.org/)
