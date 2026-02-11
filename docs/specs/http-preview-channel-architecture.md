# HTTP Preview Channel Architecture

> **Status**: CANONICAL PLAN - Do not deviate without explicit approval
> **Created**: 2026-01-29
> **Purpose**: Browser-to-agent HTTP proxying for dev server preview

---

## Goals

1. **Lazy-loaded HTTP tunneling**: Browser requests preview, CLI spins up forwarding on demand
2. **E2E encrypted**: HTTP traffic encrypted via Signal Protocol, Rails is a blind relay
3. **Clean ownership model**: BrowserClient owns preview channels, not Agent or Hub
4. **Port isolation**: Each PTY session owns its port, Hub allocates unique ports

---

## Non-Goals

- Hub does NOT manage preview channels
- Agent does NOT own preview channels (legacy code to be removed)
- No persistent tunnel - channels created on-demand per browser session

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              BROWSER                                         │
│  ┌─────────────────┐    ┌──────────────────┐    ┌────────────────────────┐  │
│  │  Preview iframe │───▶│  Service Worker  │───▶│  preview_controller.js │  │
│  │  (dev server)   │    │  (fetch intercept)│    │  (Signal encryption)   │  │
│  └─────────────────┘    └──────────────────┘    └───────────┬────────────┘  │
└─────────────────────────────────────────────────────────────┼───────────────┘
                                                              │
                                          ActionCable (PreviewChannel)
                                          [encrypted HTTP request]
                                                              │
┌─────────────────────────────────────────────────────────────┼───────────────┐
│                           RAILS SERVER                      │               │
│  ┌──────────────────────────────────────────────────────────┼────────────┐  │
│  │                     PreviewChannel                       ▼            │  │
│  │  • Browser subscribes with browser_identity                           │  │
│  │  • Agent subscribes without browser_identity                          │  │
│  │  • Rails relays encrypted blobs (cannot read content)                 │  │
│  │  • On browser subscribe: creates browser_wants_preview HubCommand     │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┬───────────────┘
                                                              │
                                           HubCommand queue
                                           (browser_wants_preview)
                                                              │
┌─────────────────────────────────────────────────────────────┼───────────────┐
│                            RUST CLI                         ▼               │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         ServerComms                                     │ │
│  │  • Receives browser_wants_preview                                       │ │
│  │  • Broadcasts HubEvent::HttpConnectionRequested                         │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                        BrowserClient                                    │ │
│  │  • Handles HttpConnectionRequested                                      │ │
│  │  • Creates HttpChannel for (agent_index, pty_index)                     │ │
│  │  • Owns http_channels: HashMap<(agent_index, pty_index), HttpChannel>   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                          HttpChannel                                    │ │
│  │  • Subscribes to ActionCable PreviewChannel (agent side)                │ │
│  │  • Queries PtyHandle for port                                           │ │
│  │  • Owns HttpProxy instance configured with port                         │ │
│  │  • Request loop: decrypt → proxy to localhost:PORT → encrypt response   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                          HttpProxy                                      │ │
│  │  • HTTP client (reqwest)                                                │ │
│  │  • Forwards to localhost:PORT                                           │ │
│  │  • Handles compression, hop-by-hop headers                              │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                    PtySession (server PTY, index=1)                     │ │
│  │  • Owns port: Option<u16>                                               │ │
│  │  • Sets BOTSTER_TUNNEL_PORT env var                                     │ │
│  │  • Exposes port via PtyHandle                                           │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                        │
│                                    ▼                                        │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         Dev Server                                      │ │
│  │  • Runs on localhost:PORT                                               │ │
│  │  • Receives proxied HTTP requests                                       │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow

### 1. Browser Initiates Preview

```
Browser navigates to: /hubs/:hub_id/agents/:agent_index/1/preview
                                                       ↑
                                              pty_index=1 (server PTY)
```

### 2. Channel Subscription

```
Browser                          Rails                           CLI
   │                               │                               │
   │──subscribe(PreviewChannel)───▶│                               │
   │   {hub_id, agent_index,       │                               │
   │    browser_identity}          │                               │
   │                               │                               │
   │                               │──HubCommand──────────────────▶│
   │                               │  browser_wants_preview        │
   │                               │  {agent_index,                │
   │                               │   browser_identity}           │
   │                               │                               │
   │                               │         HubEvent::HttpConnectionRequested
   │                               │                    │          │
   │                               │                    ▼          │
   │                               │              BrowserClient    │
   │                               │              creates          │
   │                               │              HttpChannel      │
   │                               │                               │
   │                               │◀──subscribe(PreviewChannel)───│
   │                               │   {hub_id, agent_index}       │
   │                               │   (no browser_identity =      │
   │                               │    agent side)                │
```

### 3. HTTP Request/Response

```
Browser                          Rails                           CLI
   │                               │                               │
   │──encrypted HTTP request──────▶│──────relay to agent──────────▶│
   │  {method, url, headers, body} │                               │
   │                               │                    HttpChannel│
   │                               │                    decrypts   │
   │                               │                        │      │
   │                               │                        ▼      │
   │                               │                    HttpProxy  │
   │                               │                    localhost  │
   │                               │                    :PORT      │
   │                               │                        │      │
   │                               │                        ▼      │
   │                               │                    Dev Server │
   │                               │                        │      │
   │                               │                        ▼      │
   │◀─encrypted HTTP response──────│◀─────relay to browser─────────│
   │  {status, headers, body}      │                               │
```

---

## Component Ownership

| Component | Owner | Responsibility |
|-----------|-------|----------------|
| Port allocation | Hub | `allocate_unique_port()` - finds open port, tracks usage |
| Port storage | PtySession | `port: Option<u16>` - stores allocated port |
| Port exposure | PtyHandle | `port()` - allows clients to query port |
| HttpChannel | BrowserClient | Creates, owns, manages lifecycle |
| HttpProxy | HttpChannel | Performs actual HTTP proxying |
| PreviewChannel (Rails) | N/A | Blind relay for encrypted messages |

---

## Struct Definitions

### HttpChannel (new)

```rust
/// HTTP channel for preview proxying.
/// Similar to TerminalChannel but handles HTTP request/response.
struct HttpChannel {
    /// ActionCable channel subscribed to PreviewChannel (agent side)
    channel: ActionCableChannel,

    /// HTTP proxy configured with port from PtyHandle
    http_proxy: HttpProxy,

    /// Task handling incoming requests from browser
    input_task: JoinHandle<()>,

    /// Task sending responses back to browser
    output_task: JoinHandle<()>,
}

impl HttpChannel {
    /// Create new HttpChannel for a specific agent/pty combo.
    /// Queries PtyHandle for port, creates HttpProxy, subscribes to channel.
    async fn new(
        agent_index: usize,
        pty_index: usize,
        pty_handle: &PtyHandle,
        config: &BrowserClientConfig,
    ) -> Result<Self, String>;
}
```

### HubEvent (addition)

```rust
pub enum HubEvent {
    // ... existing variants ...

    /// Browser requested HTTP preview connection
    HttpConnectionRequested {
        client_id: ClientId,
        agent_index: usize,
        pty_index: usize,
        browser_identity: String,
    },
}
```

### PtySession (additions)

```rust
pub struct PtySession {
    // ... existing fields ...

    /// Allocated port for HTTP forwarding (server PTY only)
    port: Option<u16>,
}

impl PtySession {
    pub fn set_port(&mut self, port: u16);
    pub fn port(&self) -> Option<u16>;
}
```

### BrowserClient (additions)

```rust
pub struct BrowserClient {
    // ... existing fields ...

    /// HTTP channels for preview proxying, keyed by (agent_index, pty_index)
    http_channels: HashMap<(usize, usize), HttpChannel>,
}
```

---

## Routes

| Route | Purpose |
|-------|---------|
| `/hubs/:hub_id/agents/:agent_index/:pty_index/preview` | Preview page (uses preview_controller.js) |
| `/hubs/:hub_id/agents/:agent_index/:pty_index/preview/sw.js` | Service worker |
| `/hubs/:hub_id/agents/:agent_index/:pty_index/preview/*path` | Catch-all for proxied paths |

For now, pty_index is hardcoded to `1` (server PTY) in the GUI button.

---

## Files to Modify

### Create
- `cli/src/client/http_channel.rs` - HttpChannel implementation

### Modify
- `cli/src/hub/mod.rs` - Add `allocate_unique_port()` method
- `cli/src/agent/pty/mod.rs` - Add port field to PtySession
- `cli/src/hub/agent_handle.rs` - Expose port via PtyHandle
- `cli/src/hub/lifecycle.rs` - Use Hub port allocation, pass to spawn
- `cli/src/hub/server_comms.rs` - Route browser_wants_preview to HubEvent
- `cli/src/hub/mod.rs` (HubEvent) - Add HttpConnectionRequested variant
- `cli/src/client/browser.rs` - Handle HttpConnectionRequested, manage http_channels
- `app/views/hubs/agents/show.html.erb` - Add Preview button

### Delete
- `app/javascript/controllers/preview_proxy_controller.js` - Uses wrong channel
- `cli/src/agent/mod.rs` - Remove preview_channel, tunnel_port fields and methods

---

## Security Model

1. **E2E Encryption**: All HTTP traffic encrypted with Signal Protocol
2. **Rails is blind**: Server relays encrypted blobs, cannot inspect content
3. **Localhost only**: HttpProxy only forwards to 127.0.0.1
4. **Per-session channels**: Each browser session gets its own encrypted channel

---

## Constraints

1. **No deviation** from this architecture without explicit approval
2. **BrowserClient owns HttpChannel** - not Agent, not Hub
3. **PtySession owns port** - not Agent
4. **Hub allocates ports** - centralized to prevent conflicts
5. **Lazy loading** - channels created on-demand, not at agent spawn

---

## Implementation Order

```
Phase 1: Port Infrastructure
  #7  Hub.allocate_unique_port()
  #8  PtySession.port field + PtyHandle exposure
  #9  Update server PTY spawn

Phase 2: HttpChannel
  #10 Create HttpChannel type
  #11 Add HubEvent::HttpConnectionRequested
  #12 Route browser_wants_preview to HubEvent
  #13 BrowserClient handles HttpConnectionRequested

Phase 3: Cleanup & GUI
  #1  Delete preview_proxy_controller.js
  #14 Remove old Agent.preview_channel code
  #6  Add Preview button to terminal header
```

---

## Testing Strategy

1. **Unit tests**: Port allocation, HttpChannel creation
2. **Integration tests**: Full flow from browser_wants_preview to HttpProxy
3. **E2E tests**: Browser → Rails → CLI → Dev Server round trip

---

## Open Questions (Resolved)

| Question | Resolution |
|----------|------------|
| Who allocates ports? | Hub |
| Who stores ports? | PtySession |
| Who owns HttpChannel? | BrowserClient |
| How does HttpChannel get port? | Queries PtyHandle |
| What triggers channel creation? | browser_wants_preview → HubEvent |
