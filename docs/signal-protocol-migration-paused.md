# Signal Protocol Migration - Paused

**Date**: 2025-01-08
**Status**: Paused in favor of exploring Headscale/DERP approach
**Stash Reference**: `signal-protocol-libsignal-migration`

## Goal

Replace vodozemac/Olm encryption with libsignal for full Signal Protocol implementation:
- X3DH key agreement
- Double Ratchet messaging
- Kyber post-quantum cryptography
- SenderKey group messaging (CLI broadcast to multiple browsers)

## What Was Completed

### Phase 1: CLI libsignal Integration (DONE)

**Files created:**
- `cli/src/relay/signal.rs` - SignalProtocolManager wrapper
- `cli/src/relay/signal_stores.rs` - All 6 store trait implementations

**Files modified:**
- `cli/Cargo.toml` - Added libsignal-protocol dependency
- `cli/src/relay/mod.rs` - Export signal instead of olm
- `cli/src/relay/types.rs` - SignalEnvelope instead of OlmEnvelope
- `cli/src/relay/connection.rs` - Refactored for non-Send futures (spawn_local + select!)
- `cli/src/relay/state.rs` - PreKeyBundleData instead of SessionEstablishmentKeys
- `cli/src/relay/persistence.rs` - Signal store persistence with AES-256-GCM
- `cli/src/hub/registration.rs` - Use SignalProtocolManager
- `cli/src/hub/actions.rs` - URL fragment with base64 PreKeyBundle
- `cli/src/device.rs` - Fixed rand 0.9 API changes

**Key technical challenges solved:**
- `async_trait(?Send)` - libsignal futures are not Send
- Multiple mutable borrows - solved with Arc<RwLock<...>> cloning pattern
- `tokio::spawn` requires Send - refactored to `spawn_local` + `select!`
- rand 0.9 API changes (`thread_rng()` → `rng()`)

**All 295 CLI library tests pass.**

### Phase 2: libsignal-wasm Crate (PARTIAL)

**Files created:**
- `libsignal-wasm/Cargo.toml` - WASM crate configuration
- `libsignal-wasm/.cargo/config.toml` - getrandom WASM config
- `libsignal-wasm/src/lib.rs` - SignalSession WASM wrapper
- `libsignal-wasm/src/stores.rs` - BrowserSignalStore (Rc<RefCell<...>> for single-threaded WASM)

**Status:** Compiles to WASM successfully, but API mismatches with latest libsignal remain:
- `PreKeyBundle::new` signature changed (10 args, Kyber separate)
- `IdentityKeyStore::save_identity` returns `IdentityChange` not `bool`
- `KyberPreKeyStore::mark_kyber_pre_key_used` takes 4 params now
- `group_decrypt` expects `&[u8]` not `&SenderKeyMessage`
- Record serialization requires `GenericSignedPreKey` trait import
- `DeviceId` API changed

### Not Started

- Phase 3: Browser JavaScript (`app/javascript/crypto/signal.js`)
- Phase 4: SenderKey group messaging integration
- Phase 5: Cleanup (delete vodozemac-wasm, olm.js)

## Files to Delete (if resuming)

When resuming this work, these old files should be removed:
- `cli/src/relay/olm.rs`
- `vodozemac-wasm/` (entire directory)
- `app/javascript/crypto/olm.js`
- `public/wasm/vodozemac_wasm*`
- `app/javascript/wasm/vodozemac_wasm.js`

## Why Paused

Exploring Headscale/DERP as alternative architecture:
- Browser connects via WebSocket to DERP relay
- DERP uses Noise Protocol (same as WireGuard) - already E2E encrypted
- Tailscale infrastructure is audited and battle-tested
- Simpler than implementing Signal Protocol at application layer
- Reference: https://github.com/rickli-cloud/headscale-console

## To Resume

```bash
git stash list  # Find the stash
git stash apply stash^{/signal-protocol-libsignal-migration}
```

Then fix the remaining API mismatches in `libsignal-wasm/src/stores.rs` and `libsignal-wasm/src/lib.rs`.
