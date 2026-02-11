# Share Hub Implementation Plan (Phase 1)

## Status: IMPLEMENTED (2026-01-12)

Parts A, B, C implemented. Part D (testing) in progress.

> **Context for future sessions:** This plan enables sharing hub connections between devices
> without re-scanning QR codes. Server sees NOTHING - bundles stay in URL fragments.

## Current Codebase Structure (Key Files)

```
cli/src/relay/
├── types.rs        # BrowserEvent enum (line 225), BrowserCommand, messages
├── events.rs       # command_to_event(), BrowserEventContext
├── state.rs        # BrowserState struct (line 32), single session currently
├── browser.rs      # poll_events_headless(), event handlers
├── signal_stores.rs # PreKey storage, Signal Protocol stores
├── connection.rs   # WebSocket/Action Cable connection
└── mod.rs          # Public exports

app/javascript/controllers/
├── connection_controller.js  # Signal session, send(), handleDecryptedMessage()
└── agents_controller.js      # Agent list UI
```

## Goal

Allow a user to share a hub connection with another device WITHOUT that device scanning the CLI's QR code directly.

**Flow:**
1. User has Phone connected to CLI (scanned QR)
2. User clicks "Share Hub" on Phone
3. Phone requests fresh PreKeyBundle from CLI
4. Phone generates shareable link: `https://trybotster.com/hubs/{id}#bundle={base64}`
5. User sends link to self (iMessage, email, etc.)
6. Computer opens link, establishes session with CLI
7. Both devices can now receive from CLI

**Privacy Constraint:** Server sees NOTHING. Bundle stays in URL fragment (`#`), never sent to server.

---

## Part A: CLI Generates Invite Bundles

### Files to Modify

**`cli/src/relay/mod.rs`** or **`cli/src/relay/browser.rs`**

### New Message Types

```rust
// Browser → CLI (request)
{
    "type": "generate_invite"
}

// CLI → Browser (response)
{
    "type": "invite_bundle",
    "bundle": {
        "identity_key": "base64...",
        "signed_pre_key": { "key_id": 1, "public_key": "base64...", "signature": "base64..." },
        "one_time_pre_key": { "key_id": 42, "public_key": "base64..." },
        "kyber_pre_key": { "key_id": 1, "public_key": "base64...", "signature": "base64..." }
    }
}
```

### Implementation

1. In `poll_events_headless()` or equivalent, handle `BrowserEvent::GenerateInvite`
2. Call into Signal stores to generate a fresh one-time PreKey
3. Build PreKeyBundle (same format as QR code bundle)
4. Send back via encrypted channel

### Key Files to Reference

- `cli/src/relay/signal_stores.rs` - PreKey storage
- `cli/src/relay/mod.rs` - Bundle generation for QR (reuse this logic)
- `cli/src/hub/registration.rs` - How bundle is currently created

---

## Part B: Browser UI - Share Button

### Files to Modify

**`app/views/hubs/show.html.erb`** - Add Share button
**`app/javascript/controllers/connection_controller.js`** - Add share methods

### UI Location

Add "Share Hub" button near the connection status or in a hub settings menu.

### JavaScript Implementation

```javascript
// In connection_controller.js

async requestInviteBundle() {
    // Send request to CLI via encrypted channel
    await this.send("generate_invite");
    // Response handled in handleDecryptedMessage()
}

handleDecryptedMessage(message) {
    // ... existing cases ...

    if (message.type === "invite_bundle") {
        this.handleInviteBundle(message.bundle);
        return;
    }
}

handleInviteBundle(bundle) {
    // Encode bundle as base64
    const bundleJson = JSON.stringify(bundle);
    const bundleBase64 = btoa(bundleJson);

    // Build shareable URL
    const url = `${window.location.origin}/hubs/${this.hubIdentifier}#bundle=${bundleBase64}`;

    // Try native share, fall back to clipboard
    if (navigator.share) {
        navigator.share({
            title: 'Join Hub',
            text: 'Connect to my Botster hub',
            url: url
        });
    } else {
        navigator.clipboard.writeText(url);
        // Show "Copied!" toast
    }
}
```

### Stimulus Action

```html
<button data-action="connection#requestInviteBundle">
    Share Hub
</button>
```

---

## Part C: CLI Multi-Session Support

### Current State

CLI currently assumes ONE browser session:
- `cli/src/relay/browser.rs` - Single `SignalSession`
- Messages encrypted for single recipient

### Required Changes

**`cli/src/relay/browser.rs`** or new **`cli/src/relay/sessions.rs`**

```rust
// Current (single session)
struct Browser {
    session: Option<SignalSession>,
    // ...
}

// New (multiple sessions)
struct Browser {
    sessions: HashMap<String, SignalSession>,  // keyed by browser identity
    // ...
}
```

### Broadcasting Messages

When CLI sends output to browsers:

```rust
// Current
fn send_to_browser(&self, message: &str) {
    if let Some(session) = &self.session {
        let encrypted = session.encrypt(message);
        self.send(encrypted);
    }
}

// New
fn broadcast_to_browsers(&self, message: &str) {
    for (id, session) in &self.sessions {
        let encrypted = session.encrypt(message);
        self.send_to(id, encrypted);
    }
}
```

### Session Identification

Each browser needs a unique ID. Options:
1. Use browser's identity key as ID (derived from Signal session)
2. Generate UUID on connect
3. Use connection ID from Action Cable

Recommend: Use identity key - already unique per browser.

### Handling New Connections

When browser connects with a bundle generated by `generate_invite`:
1. CLI establishes new Signal session (X3DH with the fresh PreKey)
2. Adds to `sessions` HashMap
3. Sends scrollback sync to new browser
4. New browser receives future broadcasts

### Files to Modify

- `cli/src/relay/browser.rs` - Multi-session HashMap
- `cli/src/relay/mod.rs` - Broadcast logic
- `cli/src/hub/mod.rs` - Session management

---

## Part D: Testing

### System Test

```ruby
# test/system/terminal_relay_test.rb

test "second browser connects via shared invite link" do
    @cli = start_cli(@hub, timeout: 20)

    # First browser connects normally
    sign_in_as(@user)
    visit @cli.connection_url
    assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20

    # Request invite bundle
    click_button "Share Hub"

    # Get the generated URL (from clipboard or intercept)
    invite_url = page.evaluate_script("navigator.clipboard.readText()")

    # Second browser (new window) uses invite
    new_window = open_new_window
    within_window new_window do
        visit invite_url
        assert_selector "[data-connection-target='status']", text: /connected/i, wait: 20
    end

    # Both should be connected
    assert_selector "[data-connection-target='status']", text: /connected/i
    within_window new_window do
        assert_selector "[data-connection-target='status']", text: /connected/i
    end
end
```

---

## Implementation Order

1. **Part A first** - CLI can generate bundles (backend ready)
2. **Part C second** - CLI handles multiple sessions (required for step 4)
3. **Part B third** - Browser UI to request and share
4. **Part D last** - Test the full flow

---

## Security Notes

- Each invite bundle contains a FRESH one-time PreKey
- Server never sees bundle (stays in URL fragment)
- Each browser has independent Signal session with CLI
- If invite link is intercepted, attacker could connect BUT:
  - They'd need to be authenticated with Rails (signed in)
  - CLI could limit max sessions
  - Link could be made single-use (CLI tracks used PreKey IDs)

---

## Future: SenderKey Optimization (Phase 2)

Current approach: CLI encrypts message N times (once per browser).
SenderKey approach: CLI encrypts once, all browsers decrypt with shared key.

For 2-3 browsers, N encryptions is fine. SenderKey matters at scale.
