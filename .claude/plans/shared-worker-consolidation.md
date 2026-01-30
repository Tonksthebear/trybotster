# SharedWorker Consolidation: Interface Contract

## Overview

Consolidate Signal encryption and ActionCable connections into a single SharedWorker. Main thread consumers (Stimulus controllers) continue using ConnectionManager with the same interface—implementation changes to route through the worker.

## Architecture

```
┌────────────────────────────────────────────────────────────────────────────┐
│                           SharedWorker                                      │
│                                                                            │
│  ┌──────────────┐  ┌──────────────────┐  ┌───────────────────────────────┐ │
│  │ Port         │  │ Connection Pool  │  │ Signal Crypto                 │ │
│  │ Registry     │  │                  │  │                               │ │
│  │              │  │ hubId → {        │  │ - WASM module                 │ │
│  │ portId → {   │  │   cable,         │  │ - Session management          │ │
│  │   port,      │  │   refCount,      │  │ - Encrypt/decrypt with mutex  │ │
│  │   lastPong,  │  │   state,         │  │                               │ │
│  │   hubRefs    │  │   subscriptions  │  │                               │ │
│  │ }            │  │ }                │  │                               │ │
│  └──────────────┘  └──────────────────┘  └───────────────────────────────┘ │
│         │                   │                         │                    │
│         └───────────────────┼─────────────────────────┘                    │
│                             ▼                                              │
│              ┌──────────────────────────────┐                              │
│              │ Channel Layer                │                              │
│              │                              │                              │
│              │ - Reliable delivery (seq/ack)│                              │
│              │ - Encryption integration     │                              │
│              │ - Message routing to ports   │                              │
│              └──────────────────────────────┘                              │
│                             │                                              │
│                             ▼                                              │
│                    [ ActionCable WebSocket ]                               │
└────────────────────────────────────────────────────────────────────────────┘
            ↑                           ↑                          ↑
         port.postMessage            port.postMessage           port.postMessage
            │                           │                          │
     ┌──────┴──────┐            ┌───────┴──────┐           ┌───────┴──────┐
     │    Tab 1    │            │    Tab 2     │           │    Tab 3     │
     │             │            │              │           │              │
     │ Connection  │            │ Connection   │           │ Connection   │
     │ Manager     │            │ Manager      │           │ Manager      │
     │      ↓      │            │      ↓       │           │      ↓       │
     │ Stimulus    │            │ Stimulus     │           │ Stimulus     │
     │ Controllers │            │ Controllers  │           │ Controllers  │
     └─────────────┘            └──────────────┘           └──────────────┘
```

---

## Message Protocol

### Request/Response Pattern

All requests from main thread include an `id` for correlation (same pattern as existing Signal worker). Worker responds with same `id`.

**Request (Main → Worker):**
```javascript
{
  id: 1,           // Unique request ID for correlation
  action: "connect",  // Action name
  // ...action-specific parameters
}
```

**Response (Worker → Main):**
```javascript
{
  id: 1,           // Correlates to request
  success: true,
  result: { ... }  // On success
  // or
  error: "message" // On failure
}
```

### Event Pattern (Unsolicited)

Worker pushes events without a request ID.

**Event (Worker → Main):**
```javascript
{
  event: "subscription:message",
  hubId: "abc123",
  subscriptionId: "sub_1",
  data: { ... }
}
```

---

## Worker Actions

### Initialization

#### `init`
Initialize WASM module. Idempotent.

```javascript
// Request
{ id: 1, action: "init", wasmJsUrl: "...", wasmBinaryUrl: "..." }

// Response
{ id: 1, success: true }
```

### Connection Management

#### `connect`
Establish or reuse ActionCable connection to a hub. Increments refcount for this port.

```javascript
// Request
{
  id: 2,
  action: "connect",
  hubId: "abc123",
  cableUrl: "/cable",              // ActionCable WebSocket URL
  sessionBundle: { ... },          // Optional: create session if not exists
}

// Response
{
  id: 2,
  success: true,
  result: {
    state: "connecting", // or "connected"
    sessionExists: true
  }
}
```

**Behavior:**
- If connection exists for hubId, increment refcount for this port
- If no connection, create ActionCable consumer and connect
- If sessionBundle provided and no session exists, create Signal session
- Worker tracks which ports have refs to which hubs

#### `disconnect`
Release connection reference. Worker closes connection when refcount hits 0.

```javascript
// Request
{ id: 3, action: "disconnect", hubId: "abc123" }

// Response
{ id: 3, success: true, result: { refCount: 0, closed: true } }
```

**Behavior:**
- Decrement refcount for this port's hub reference
- If total refcount across all ports is 0, close ActionCable connection
- Unsubscribe all subscriptions owned by this port for this hub

### Subscription Management

#### `subscribe`
Subscribe to a channel. Messages are routed to the requesting port.

```javascript
// Request
{
  id: 4,
  action: "subscribe",
  hubId: "abc123",
  channel: "HubChannel",           // or "TerminalRelayChannel"
  params: { hub_id: "abc123" },    // Channel params
  reliable: true,                  // Enable reliable delivery
}

// Response
{
  id: 4,
  success: true,
  result: { subscriptionId: "sub_1" }
}
```

**Behavior:**
- Create ActionCable subscription
- If reliable=true, create ReliableSender/Receiver for this subscription
- Route all received messages (after decryption) to this port
- Subscription owned by requesting port

#### `unsubscribe`
Remove a subscription.

```javascript
// Request
{ id: 5, action: "unsubscribe", subscriptionId: "sub_1" }

// Response
{ id: 5, success: true }
```

### Messaging

#### `send`
Send a message on a subscription. Worker handles encryption and reliable delivery.

```javascript
// Request
{
  id: 6,
  action: "send",
  subscriptionId: "sub_1",
  message: { type: "input", data: "ls -la" },  // Plaintext
}

// Response
{ id: 6, success: true, result: { seq: 42 } }
```

**Behavior:**
- If subscription has reliable delivery, wrap with sequence number
- Encrypt via Signal session
- Send via ActionCable subscription
- Handle retransmission internally if reliable

#### `perform`
Execute an ActionCable action (for channels that use perform pattern).

```javascript
// Request
{
  id: 7,
  action: "perform",
  subscriptionId: "sub_1",
  actionName: "speak",
  data: { message: "hello" },
}

// Response
{ id: 7, success: true }
```

### Session Management (Existing - Unchanged)

```javascript
// createSession - Create from PreKeyBundle
{ id: 8, action: "createSession", hubId: "abc123", bundle: { ... } }

// loadSession - Load from IndexedDB
{ id: 9, action: "loadSession", hubId: "abc123" }

// hasSession - Check existence
{ id: 10, action: "hasSession", hubId: "abc123" }

// clearSession - Delete
{ id: 11, action: "clearSession", hubId: "abc123" }

// getIdentityKey - Get public key
{ id: 12, action: "getIdentityKey", hubId: "abc123" }

// processSenderKeyDistribution - Handle group key
{ id: 13, action: "processSenderKeyDistribution", hubId: "abc123", distributionMessage: { ... } }
```

---

## Worker Events

Events pushed to ports without a corresponding request.

### Connection Events

#### `connection:state`
Connection state changed.

```javascript
{
  event: "connection:state",
  hubId: "abc123",
  state: "connected",  // or "connecting", "disconnected"
  reason: "closed"     // On disconnect: "closed", "rejected", "error"
}
```

#### `connection:error`
Connection-level error.

```javascript
{
  event: "connection:error",
  hubId: "abc123",
  error: {
    type: "rejected",  // or "timeout", "websocket_error"
    message: "Authentication failed"
  }
}
```

### Subscription Events

#### `subscription:confirmed`
ActionCable confirmed subscription.

```javascript
{
  event: "subscription:confirmed",
  subscriptionId: "sub_1"
}
```

#### `subscription:rejected`
ActionCable rejected subscription.

```javascript
{
  event: "subscription:rejected",
  subscriptionId: "sub_1",
  reason: "Not authorized"
}
```

#### `subscription:message`
Decrypted message received. This is the main data flow.

```javascript
{
  event: "subscription:message",
  subscriptionId: "sub_1",
  message: { type: "output", data: "total 42\n" }  // Decrypted, decompressed, in-order
}
```

### Session Events

#### `session:invalid`
Decryption failures exceeded threshold. Session needs re-establishment.

```javascript
{
  event: "session:invalid",
  hubId: "abc123",
  message: "Encryption session expired. Please re-scan the QR code to reconnect."
}
```

---

## Main Thread Classes

### ConnectionManager (Public API - Unchanged)

Stimulus controllers continue using this interface. Implementation changes internally.

```javascript
class ConnectionManager {
  // Acquire connection - increments refcount, returns when connected
  async acquire(identifier) { }  // Returns ManagedConnection

  // Release connection - decrements refcount
  release(identifier) { }

  // Get without acquiring (for read-only access)
  get(identifier) { }  // Returns ManagedConnection | undefined

  // Passive subscribers (don't affect refcount)
  addSubscriber(identifier, callback) { }
  removeSubscriber(identifier, callback) { }
}
```

### ManagedConnection

Thin wrapper that proxies to worker.

```javascript
class ManagedConnection {
  hubId        // readonly
  state        // "connecting" | "connected" | "disconnected"

  // Subscribe to a channel on this connection
  async subscribe(options) { }  // Returns Subscription

  // State change callback
  onStateChange(callback) { }  // Returns unsubscribe function

  // Error callback
  onError(callback) { }  // Returns unsubscribe function
}

// subscribe options
{
  channel: "HubChannel",
  params: { hub_id: "abc123" },
  reliable: true,
  onMessage: (message) => { }
}
```

### Subscription

```javascript
class Subscription {
  id       // readonly
  channel  // readonly

  // Send message (worker encrypts + handles reliable delivery)
  async send(message) { }

  // ActionCable perform
  async perform(action, data) { }

  // Cleanup
  unsubscribe() { }
}
```

---

## Worker Internal State

```javascript
// Private fields use # prefix

class WorkerState {
  #ports = new Map()        // portId → { port, lastPong, hubRefs, subscriptions }
  #connections = new Map()  // hubId → { cable, state, refCount, portRefs, subscriptions }
  #sessions = new Map()     // hubId → encrypted session state
  #operationQueues = new Map()  // hubId → Promise (mutex)
  #wasmModule = null
  #wrappingKey = null
}

// Port entry structure
{
  port: MessagePort,
  lastPong: Date,
  hubRefs: Set(),           // Which hubs this port has connected to
  subscriptions: Set()      // Subscription IDs owned by this port
}

// Connection entry structure
{
  cable: ActionCable.Consumer,
  state: "connecting",      // or "connected", "disconnected"
  refCount: 2,              // Total refs across all ports
  portRefs: Map(),          // portId → refcount (for cleanup on port death)
  subscriptions: Map()      // subscriptionId → { channel, portId, reliable }
}
```

---

## Port Lifecycle

### Connection
```javascript
// Worker: onconnect handler
self.onconnect = (event) => {
  const port = event.ports[0]
  const portId = crypto.randomUUID()

  this.#ports.set(portId, {
    port,
    lastPong: new Date(),
    hubRefs: new Set(),
    subscriptions: new Set()
  })

  port.onmessage = (e) => this.#handleMessage(portId, e.data, port)
  port.start()
}
```

### Heartbeat
```javascript
// Worker: ping all ports every 5 seconds
setInterval(() => {
  const now = new Date()

  for (const [portId, state] of this.#ports) {
    // Check for dead ports (no pong in 21 seconds)
    if (now - state.lastPong > 21000) {
      this.#cleanupPort(portId)
      continue
    }

    state.port.postMessage({ event: "ping" })
  }
}, 5000)

// Main thread: respond to ping
workerPort.onmessage = (e) => {
  if (e.data.event === "ping") {
    workerPort.postMessage({ action: "pong" })
    return
  }
  // ... handle other messages
}
```

### Cleanup on Port Death
```javascript
#cleanupPort(portId) {
  const state = this.#ports.get(portId)
  if (!state) return

  // Unsubscribe all subscriptions owned by this port
  for (const subId of state.subscriptions) {
    this.#unsubscribeInternal(subId)
  }

  // Release all hub refs
  for (const hubId of state.hubRefs) {
    this.#releaseConnection(portId, hubId)
  }

  this.#ports.delete(portId)
}
```

---

## Migration Notes

### What Moves to Worker
- `app/javascript/channels/channel.js` → Worker (Channel + encryption integration)
- `app/javascript/channels/reliable_channel.js` → Worker (ReliableSender/Receiver)
- `app/javascript/channels/secure_channel.js` → Worker (open/loadSession logic)
- ActionCable consumer creation → Worker

### What Stays in Main Thread
- `ConnectionManager` (interface unchanged, implementation rewired)
- `HubConnection` / `TerminalConnection` / `PreviewConnection` (become thin message handlers)
- Stimulus controllers (unchanged)

### What Gets Deleted
- `app/javascript/channels/consumer.js` (worker owns consumer)
- Direct ActionCable imports in connection classes

---

## Error Handling

### Decryption Failure Recovery
Worker tracks consecutive decryption failures per hub. After threshold (5):
1. Emit `session:invalid` event to all ports with refs to that hub
2. Clear local session state
3. Main thread shows "re-scan QR" UI

### Connection Loss Recovery
Worker handles reconnection internally:
1. ActionCable has built-in reconnect
2. On reconnect, worker re-subscribes all active subscriptions
3. Reliable delivery resumes (pending messages retransmitted)

### Port Death During Send
If port dies while reliable message pending ACK:
1. Worker detects port death via heartbeat
2. Cleans up subscription
3. Stops retransmission (no one to notify)

---

## Sequence Diagrams

### Initial Connection Flow
```
Tab                    Worker                    Server
 │                       │                         │
 │──connect(hubId)──────▶│                         │
 │                       │──ActionCable.connect───▶│
 │                       │◀──WebSocket open────────│
 │                       │                         │
 │◀──{success, state}────│                         │
 │                       │                         │
 │◀──connection:state────│                         │
 │   (connected)         │                         │
```

### Subscribe + Message Flow
```
Tab                    Worker                    Server
 │                       │                         │
 │──subscribe(channel)──▶│                         │
 │                       │──subscriptions.create──▶│
 │                       │◀──confirm───────────────│
 │◀──{subscriptionId}────│                         │
 │◀──subscription:confirmed                        │
 │                       │                         │
 │                       │◀──encrypted message─────│
 │                       │  [decrypt]              │
 │                       │  [decompress]           │
 │                       │  [reorder if reliable]  │
 │◀──subscription:message│                         │
```

### Send Message Flow
```
Tab                    Worker                    Server
 │                       │                         │
 │──send(subId, msg)────▶│                         │
 │                       │  [wrap with seq#]       │
 │                       │  [encrypt]              │
 │                       │──ActionCable.send──────▶│
 │◀──{success, seq}──────│                         │
 │                       │                         │
 │                       │◀──SACK──────────────────│
 │                       │  [mark delivered]       │
```

### Multi-Tab Sharing
```
Tab1                   Worker                    Tab2
 │                       │                         │
 │──connect(hub1)───────▶│                         │
 │                       │  [create connection]    │
 │◀──connected───────────│                         │
 │                       │                         │
 │                       │◀──connect(hub1)─────────│
 │                       │  [increment refcount]   │
 │                       │──connected─────────────▶│
 │                       │                         │
 │                       │  [same WebSocket!]      │
```
