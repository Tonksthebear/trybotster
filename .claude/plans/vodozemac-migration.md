# Vodozemac Migration Plan - Battle-Tested E2E Encryption

## Overview

Replace our custom Double Ratchet implementation with [vodozemac](https://github.com/matrix-org/vodozemac), Matrix's audited Olm/Megolm implementation. We'll create WASM bindings to use the same battle-tested Rust code on both CLI and browser.

## Why Vodozemac

- **Audited**: NCC Group security audit
- **Battle-tested**: Powers Matrix/Element with millions of users
- **Same code everywhere**: Rust compiles to native (CLI) and WASM (browser)
- **Maintained**: Active development by Matrix.org
- **Olm protocol**: Based on Double Ratchet, equivalent security to Signal

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        vodozemac (Rust)                         │
│                   Audited Olm Implementation                    │
└─────────────────────────────────────────────────────────────────┘
                    │                           │
                    ▼                           ▼
        ┌───────────────────┐       ┌───────────────────────────┐
        │   CLI (Native)    │       │  vodozemac-wasm (New)     │
        │                   │       │  wasm-bindgen bindings    │
        │ cargo dependency  │       │                           │
        └───────────────────┘       └───────────────────────────┘
                                                │
                                                ▼
                                    ┌───────────────────────────┐
                                    │   Browser (WASM)          │
                                    │   npm package             │
                                    └───────────────────────────┘
```

## Olm Protocol Flow

Unlike our current simple key exchange, Olm uses a more sophisticated protocol:

```
1. Account Creation (one-time setup)
   ├── CLI creates Account → generates identity keys (Ed25519 + Curve25519)
   └── Browser creates Account → generates identity keys

2. One-Time Key Generation
   ├── CLI generates one-time keys (for receiving sessions)
   └── Browser generates one-time keys

3. Session Establishment (via QR code)
   ├── QR contains: CLI's identity key + one-time key + Curve25519 key
   ├── Browser scans → creates outbound session using CLI's keys
   ├── Browser sends PreKey message (first encrypted message)
   └── CLI receives → creates inbound session → can now decrypt

4. Encrypted Communication
   ├── Both sides have matching Session objects
   ├── session.encrypt(plaintext) → OlmMessage
   └── session.decrypt(message) → plaintext
```

## Project Structure

```
trybotster/
├── cli/                          # Existing Rust CLI
│   ├── Cargo.toml               # Add vodozemac dependency
│   └── src/
│       └── relay/
│           ├── olm.rs           # NEW: Vodozemac wrapper
│           └── connection.rs    # Update to use olm.rs
│
├── vodozemac-wasm/              # NEW: WASM bindings crate
│   ├── Cargo.toml
│   ├── src/
│   │   └── lib.rs              # wasm-bindgen exports
│   ├── pkg/                    # Generated WASM package
│   └── package.json            # npm package config
│
└── app/javascript/
    └── crypto/
        ├── olm.js              # NEW: WASM wrapper
        └── ratchet.js          # DEPRECATED: Remove after migration
```

---

## Phase 1: Create vodozemac-wasm Crate

### 1.1 Initialize WASM Crate

**Files to create:**
- `vodozemac-wasm/Cargo.toml`
- `vodozemac-wasm/src/lib.rs`

**Cargo.toml:**
```toml
[package]
name = "vodozemac-wasm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
vodozemac = { version = "0.7", features = ["js"] }
wasm-bindgen = "0.2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
serde-wasm-bindgen = "0.6"
js-sys = "0.3"
getrandom = { version = "0.2", features = ["js"] }

[profile.release]
opt-level = "s"  # Optimize for size
lto = true
```

### 1.2 Implement WASM Bindings

**Types to expose:**
- `OlmAccount` - Wraps vodozemac::olm::Account
- `OlmSession` - Wraps vodozemac::olm::Session
- `OlmMessage` - Encrypted message (PreKey or Normal)
- `IdentityKeys` - Public identity keys for sharing
- `OneTimeKey` - Single-use key for session establishment

**Methods to expose:**

```rust
// Account methods
OlmAccount::new() -> OlmAccount
OlmAccount::identity_keys() -> IdentityKeys
OlmAccount::generate_one_time_keys(count: u32)
OlmAccount::one_time_keys() -> Vec<OneTimeKey>
OlmAccount::mark_keys_as_published()
OlmAccount::create_outbound_session(identity_key, one_time_key) -> OlmSession
OlmAccount::create_inbound_session(message) -> (OlmSession, Vec<u8>)
OlmAccount::sign(message) -> String
OlmAccount::pickle(key) -> String
OlmAccount::from_pickle(key, pickle) -> OlmAccount

// Session methods
OlmSession::encrypt(plaintext) -> OlmMessage
OlmSession::decrypt(message) -> Vec<u8>
OlmSession::session_id() -> String
OlmSession::pickle(key) -> String
OlmSession::from_pickle(key, pickle) -> OlmSession

// Message type
OlmMessage::message_type() -> u32  // 0 = PreKey, 1 = Normal
OlmMessage::ciphertext() -> String
```

### 1.3 Build and Package

**Build commands:**
```bash
cd vodozemac-wasm
wasm-pack build --target web --release
```

**Output:** `vodozemac-wasm/pkg/` contains:
- `vodozemac_wasm.js` - JavaScript glue
- `vodozemac_wasm_bg.wasm` - WebAssembly binary
- `vodozemac_wasm.d.ts` - TypeScript definitions
- `package.json` - npm package

---

## Phase 2: Update CLI to Use Vodozemac

### 2.1 Add Dependency

**File:** `cli/Cargo.toml`

```toml
[dependencies]
vodozemac = { version = "0.7", features = ["libolm-compat"] }
```

### 2.2 Create Olm Wrapper

**File:** `cli/src/relay/olm.rs` (NEW)

```rust
//! Olm encryption wrapper using vodozemac.
//!
//! Provides Account and Session management for E2E encrypted
//! terminal communication with browser clients.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use vodozemac::olm::{
    Account, AccountPickle, InboundCreationResult, OlmMessage,
    Session, SessionPickle, SessionConfig,
};
use vodozemac::{Curve25519PublicKey, Ed25519PublicKey};

/// Wrapper around vodozemac Account with persistence
pub struct OlmAccount {
    account: Account,
}

/// Wrapper around vodozemac Session
pub struct OlmSession {
    session: Session,
}

/// Keys needed by browser to establish session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEstablishmentKeys {
    pub ed25519_key: String,      // Identity key for signing
    pub curve25519_key: String,   // Identity key for DH
    pub one_time_key: String,     // Single-use key
}

impl OlmAccount {
    /// Create new account with fresh identity keys
    pub fn new() -> Self { ... }

    /// Load account from pickle
    pub fn from_pickle(pickle: &str, key: &[u8]) -> Result<Self> { ... }

    /// Serialize account for storage
    pub fn pickle(&self, key: &[u8]) -> String { ... }

    /// Get keys for QR code
    pub fn session_establishment_keys(&mut self) -> Result<SessionEstablishmentKeys> { ... }

    /// Create session from browser's PreKey message
    pub fn create_inbound_session(
        &mut self,
        their_identity_key: &str,
        message: &OlmMessage,
    ) -> Result<(OlmSession, Vec<u8>)> { ... }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> String { ... }
}

impl OlmSession {
    /// Encrypt plaintext
    pub fn encrypt(&mut self, plaintext: &[u8]) -> OlmMessage { ... }

    /// Decrypt message
    pub fn decrypt(&mut self, message: &OlmMessage) -> Result<Vec<u8>> { ... }

    /// Serialize for storage
    pub fn pickle(&self, key: &[u8]) -> String { ... }

    /// Restore from storage
    pub fn from_pickle(pickle: &str, key: &[u8]) -> Result<Self> { ... }
}
```

### 2.3 Update Connection Module

**File:** `cli/src/relay/connection.rs`

Changes:
- Replace `RatchetSession` with `OlmSession`
- Update `RelayState` to hold `OlmAccount` and `OlmSession`
- Update message format for Olm messages
- Handle PreKey vs Normal message types

### 2.4 Update QR Code Generation

**File:** `cli/src/qr.rs` (or wherever QR is generated)

QR code URL fragment changes from:
```
#key=<base64_public_key>&hub=<hub_id>
```

To:
```
#ed25519=<identity_key>&curve25519=<curve_key>&otk=<one_time_key>&hub=<hub_id>
```

### 2.5 Update Device Storage

**File:** `cli/src/device.rs`

- Store Olm Account pickle in keyring (encrypted)
- Store Account identity keys in device.json (public only)
- Remove old keypair storage

---

## Phase 3: Update Browser to Use WASM

### 3.1 Add WASM Package

**Option A: Local package (development)**
```javascript
// config/importmap.rb
pin "vodozemac-wasm", to: "vodozemac_wasm.js"
```

**Option B: Published to npm (production)**
```javascript
pin "@trybotster/vodozemac-wasm", to: "https://esm.sh/@trybotster/vodozemac-wasm"
```

### 3.2 Create Olm Wrapper

**File:** `app/javascript/crypto/olm.js` (NEW)

```javascript
import init, { OlmAccount, OlmSession, OlmMessage } from "vodozemac-wasm";

let wasmInitialized = false;

export async function initOlm() {
  if (!wasmInitialized) {
    await init();
    wasmInitialized = true;
  }
}

export class BrowserOlmAccount {
  constructor(account) {
    this.account = account;
  }

  static async create() {
    await initOlm();
    return new BrowserOlmAccount(OlmAccount.new());
  }

  static async fromPickle(pickle, key) {
    await initOlm();
    return new BrowserOlmAccount(OlmAccount.from_pickle(pickle, key));
  }

  pickle(key) {
    return this.account.pickle(key);
  }

  identityKeys() {
    return this.account.identity_keys();
  }

  generateOneTimeKeys(count) {
    this.account.generate_one_time_keys(count);
  }

  oneTimeKeys() {
    return this.account.one_time_keys();
  }

  markKeysAsPublished() {
    this.account.mark_keys_as_published();
  }

  createOutboundSession(identityKey, oneTimeKey) {
    const session = this.account.create_outbound_session(identityKey, oneTimeKey);
    return new BrowserOlmSession(session);
  }

  sign(message) {
    return this.account.sign(message);
  }
}

export class BrowserOlmSession {
  constructor(session) {
    this.session = session;
  }

  encrypt(plaintext) {
    return this.session.encrypt(plaintext);
  }

  decrypt(message) {
    return this.session.decrypt(message);
  }

  pickle(key) {
    return this.session.pickle(key);
  }

  static fromPickle(pickle, key) {
    return new BrowserOlmSession(OlmSession.from_pickle(pickle, key));
  }
}
```

### 3.3 Update Secure Terminal Controller

**File:** `app/javascript/controllers/secure_terminal_controller.js`

Changes:
- Import from `crypto/olm.js` instead of `crypto/ratchet.js`
- Parse new QR code format (ed25519, curve25519, otk)
- Create outbound session using CLI's keys
- Send PreKey message on first encrypt
- Handle message types (PreKey vs Normal)
- Store Account pickle in IndexedDB

### 3.4 Update IndexedDB Schema

**Changes:**
- Store Account pickle (encrypted with derived key)
- Store Session pickles by peer identity
- Bump DB_VERSION

---

## Phase 4: Update Protocol Messages

### 4.1 New Message Format

**Encrypted envelope (replaces RatchetEnvelope):**
```json
{
  "version": 3,
  "message_type": 0,        // 0 = PreKey, 1 = Normal
  "ciphertext": "base64...",
  "sender_key": "base64..." // Curve25519 sender key (for PreKey messages)
}
```

### 4.2 Presence Message Update

**Browser → CLI presence (includes PreKey message):**
```json
{
  "type": "presence",
  "event": "join",
  "device_name": "Chrome Browser",
  "identity_key": "base64...",  // Ed25519 public key
  "sender_key": "base64...",    // Curve25519 public key
  "prekey_message": {           // First encrypted message
    "message_type": 0,
    "ciphertext": "base64..."
  }
}
```

---

## Phase 5: Persistence & Session Management

### 5.1 CLI Persistence

**Account storage:**
- Pickle encrypted with key derived from device secret
- Store in `~/.config/botster/olm_account.pickle`
- Permissions: 0600

**Session storage:**
- Store active sessions in `~/.config/botster/olm_sessions/`
- One file per peer identity key
- Auto-cleanup stale sessions

### 5.2 Browser Persistence

**IndexedDB stores:**
- `olm_account`: Encrypted account pickle
- `olm_sessions`: Map of identity_key → session pickle
- `olm_pickle_key`: Key for encryption (derived from secure random)

---

## Phase 6: Cleanup

### 6.1 Remove Old Code

**Files to delete:**
- `cli/src/relay/ratchet.rs`
- `app/javascript/crypto/ratchet.js`

**Code to remove:**
- Old RatchetSession usage in connection.rs
- Old imports in secure_terminal_controller.js
- Old signing keypair code (Olm Account handles signing)

### 6.2 Update Dependencies

**CLI Cargo.toml - Remove:**
```toml
# No longer needed - vodozemac includes these
hkdf = "0.12"
aes = "0.8"
cbc = "0.1"
hmac = "0.12"
```

**Keep:**
```toml
ed25519-dalek = "2"  # Still used for device identity
```

---

## Implementation Checklist

### Phase 1: WASM Crate
- [ ] Create `vodozemac-wasm/` directory structure
- [ ] Write `Cargo.toml` with dependencies
- [ ] Implement `OlmAccount` wasm-bindgen wrapper
- [ ] Implement `OlmSession` wasm-bindgen wrapper
- [ ] Implement `OlmMessage` wrapper
- [ ] Implement `IdentityKeys` struct
- [ ] Add pickle/unpickle support
- [ ] Add error handling with proper JS exceptions
- [ ] Build with wasm-pack
- [ ] Test basic encrypt/decrypt in Node.js
- [ ] Test in browser environment

### Phase 2: CLI Integration
- [ ] Add vodozemac to Cargo.toml
- [ ] Create `cli/src/relay/olm.rs` wrapper
- [ ] Implement `OlmAccount` CLI wrapper
- [ ] Implement `OlmSession` CLI wrapper
- [ ] Update `RelayState` to use Olm types
- [ ] Update encryption in `connection.rs`
- [ ] Update decryption in `connection.rs`
- [ ] Update QR code generation format
- [ ] Update device.rs for Account storage
- [ ] Implement Account persistence (pickle)
- [ ] Implement Session persistence
- [ ] Update presence handling for PreKey messages
- [ ] Add migration from old format (if needed)
- [ ] Update tests

### Phase 3: Browser Integration
- [ ] Copy WASM package to assets
- [ ] Add to importmap.rb
- [ ] Create `app/javascript/crypto/olm.js` wrapper
- [ ] Update secure_terminal_controller.js imports
- [ ] Parse new QR code format
- [ ] Implement Account creation/loading
- [ ] Implement outbound session creation
- [ ] Update encrypt function
- [ ] Update decrypt function
- [ ] Update IndexedDB schema
- [ ] Implement Account persistence
- [ ] Implement Session persistence
- [ ] Handle PreKey vs Normal messages
- [ ] Update presence message format
- [ ] Update disconnect cleanup

### Phase 4: Protocol Updates
- [ ] Define new envelope format (version 3)
- [ ] Update CLI message parsing
- [ ] Update browser message parsing
- [ ] Update ActionCable relay handling (if needed)
- [ ] Document new protocol

### Phase 5: Testing
- [ ] Unit tests for WASM bindings
- [ ] Unit tests for CLI olm.rs
- [ ] Integration test: CLI ↔ Browser session
- [ ] Test Account persistence (CLI)
- [ ] Test Account persistence (Browser)
- [ ] Test Session persistence
- [ ] Test reconnection with existing session
- [ ] Test new device pairing
- [ ] Security test: Verify encryption is working

### Phase 6: Cleanup
- [ ] Remove `cli/src/relay/ratchet.rs`
- [ ] Remove `app/javascript/crypto/ratchet.js`
- [ ] Remove unused dependencies from Cargo.toml
- [ ] Remove old signing code (if superseded)
- [ ] Update documentation
- [ ] Update CLAUDE.md

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| WASM build issues | Medium | High | Test early, have fallback |
| Browser compatibility | Low | Medium | Test on major browsers |
| Performance (WASM size) | Low | Low | Use opt-level="s", tree shaking |
| Olm protocol mismatch | Low | High | Thorough testing, read spec |
| Migration breaks existing | N/A | N/A | Not in production |

---

## Estimated Effort

| Phase | Effort | Dependencies |
|-------|--------|--------------|
| Phase 1: WASM Crate | 4-6 hours | None |
| Phase 2: CLI Integration | 3-4 hours | Phase 1 |
| Phase 3: Browser Integration | 3-4 hours | Phase 1 |
| Phase 4: Protocol Updates | 1-2 hours | Phase 2, 3 |
| Phase 5: Testing | 2-3 hours | Phase 4 |
| Phase 6: Cleanup | 1 hour | Phase 5 |

**Total: ~15-20 hours**

---

## Success Criteria

1. **Functional**: QR scan → connect → type → output appears
2. **Secure**: All messages encrypted with vodozemac Olm
3. **Persistent**: Refresh browser → reconnects with same session
4. **Battle-tested**: Same vodozemac code on CLI and browser
5. **Clean**: No custom crypto code remaining
