# Signal Protocol E2E Encryption

This document explains our E2E encryption implementation using Signal Protocol, including important caveats about WASM compilation.

## Overview

We use the official Signal Protocol implementation from [signalapp/libsignal](https://github.com/signalapp/libsignal) for end-to-end encryption between CLI and browser.

**What we get:**
- Double Ratchet algorithm (forward secrecy)
- Post-quantum security via PQXDH (ML-KEM/Kyber)
- SenderKey for group messaging (multi-device/multi-browser)
- Same battle-tested code that Signal Messenger uses

## Important: Why This Works (And Why Signal Says It Doesn't)

### Signal's Official Position

Signal explicitly states they do not support WASM. From [GitHub Issue #350](https://github.com/signalapp/libsignal/issues/350):

> "Supporting a full wasm bridge would qualify as too much of a maintenance burden to land in the main repository."
>
> "Approaches...are much more difficult now that we have `boring` as a dependency, and possibly other C libraries in the indirect dependency tree that won't just accept a wasm target."

### Why It Actually Works For Us

The Signal repository contains **multiple crates** with different dependency trees:

| Crate | Purpose | C Dependencies | WASM Compatible |
|-------|---------|----------------|-----------------|
| `libsignal-client` | Full client library | Yes (`boring`/BoringSSL for TLS) | ❌ No |
| `libsignal-protocol` | Core Signal Protocol | No (pure Rust) | ✅ Yes |
| `signal-crypto` | Crypto primitives | No (uses RustCrypto) | ✅ Yes |

**We use only `libsignal-protocol`** - the core protocol implementation - which has no C dependencies.

The `boring` (BoringSSL) dependency that blocks WASM is in:
- `libsignal-client` (the full client with TLS support)
- `attest` (SGX remote attestation)
- Server-side components

These are **not** required for the core encryption protocol.

### What We Verified (January 2026)

We successfully compiled `libsignal-protocol` to `wasm32-unknown-unknown` target:

```
Compiling libsignal-protocol v0.1.0 (https://github.com/signalapp/libsignal)
Compiling signal-crypto v0.1.0
Compiling libcrux-ml-kem v0.0.4  # Post-quantum (formally verified)
Compiling spqr v1.3.0            # Signal's PQ ratchet
Finished `dev` profile target(s) in 14.07s
```

Key dependencies are all pure Rust:
- `libcrux-ml-kem` - Cryspen's formally verified ML-KEM (post-quantum)
- `curve25519-dalek` - X25519 key exchange
- `aes-gcm-siv` - Authenticated encryption
- RustCrypto crates (`sha2`, `hkdf`, `hmac`)

## WASM Compilation Requirements

### Configuration

To compile `libsignal-protocol` to WASM, you need specific configuration for the `getrandom` crate (used for cryptographic randomness).

**Cargo.toml:**
```toml
[dependencies]
libsignal-protocol = { git = "https://github.com/signalapp/libsignal", branch = "main" }

# Required: Enable JS random number generation for WASM
getrandom = { version = "0.2", features = ["js"] }

[target.'cfg(all(target_arch = "wasm32", target_os = "unknown"))'.dependencies]
getrandom = { version = "0.3", features = ["wasm_js"] }
```

**.cargo/config.toml:**
```toml
[target.wasm32-unknown-unknown]
rustflags = ['--cfg', 'getrandom_backend="wasm_js"']
```

### Why This Configuration

1. **`getrandom` crate**: Provides cryptographically secure random numbers
2. **WASM has no OS**: Can't use `/dev/urandom` or system calls
3. **`js` / `wasm_js` features**: Use browser's `crypto.getRandomValues()` instead
4. **Two versions**: libsignal's dependencies use both getrandom 0.2.x and 0.3.x, which have different feature names

## Risks and Caveats

### 1. Not Officially Supported

**Risk:** Signal does not officially support or test WASM compilation of `libsignal-protocol`.

**Mitigation:**
- We compile the same Rust code they use - no modifications
- The protocol logic is identical; only the random number source differs
- We should run comprehensive integration tests

### 2. No Security Audit for WASM Specifically

**Risk:** While Signal's code is battle-tested, the WASM compilation path is not audited.

**Mitigation:**
- Use `wasm-bindgen` best practices
- Ensure cryptographic operations don't leak timing information
- Consider a security review of the WASM bindings

### 3. Dependency on Git Branch

**Risk:** We depend on Signal's `main` branch, not a tagged release.

**Mitigation:**
- Pin to a specific commit hash for production
- Monitor Signal's releases and update deliberately
- Example: `{ git = "...", rev = "4cd2c507..." }`

### 4. Browser Crypto Considerations

**Risk:** Browser JavaScript environment has different security properties than native.

**Mitigation:**
- Use Web Crypto API via `getrandom` (not custom JS random)
- Be aware of side-channel risks in browser environment
- Consider Web Worker isolation for crypto operations

### 5. Post-Quantum Crypto is New

**Risk:** ML-KEM (Kyber) was standardized by NIST in 2024. Real-world attacks may emerge.

**Mitigation:**
- Signal uses hybrid approach (classical + PQ) - both must be broken
- `libcrux-ml-kem` is formally verified by Cryspen
- This is the same risk Signal Messenger accepts

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Browser                               │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  libsignal-protocol (WASM)                          │    │
│  │  - SessionBuilder, SessionCipher                    │    │
│  │  - PreKeyBundle generation/processing               │    │
│  │  - Double Ratchet encryption/decryption             │    │
│  └─────────────────────────────────────────────────────┘    │
│                            │                                 │
│                   Encrypted messages                         │
│                            ▼                                 │
└────────────────────────────┼────────────────────────────────┘
                             │
                    WebSocket (Action Cable)
                             │
                   ┌─────────▼─────────┐
                   │   Rails Server    │
                   │   (Pure Relay)    │
                   │                   │
                   │ Cannot decrypt -  │
                   │ just passes blobs │
                   └─────────┬─────────┘
                             │
                    WebSocket (Action Cable)
                             │
┌────────────────────────────▼────────────────────────────────┐
│                         CLI                                  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  libsignal-protocol (native Rust)                   │    │
│  │  - Same code as browser, native compilation         │    │
│  │  - Encrypted state persistence (AES-GCM)            │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

## Security Properties

| Property | Provided By | Status |
|----------|-------------|--------|
| **Confidentiality** | AES-GCM-SIV encryption | ✅ |
| **Authenticity** | HMAC + signatures | ✅ |
| **Forward Secrecy** | Double Ratchet (DH ratchet) | ✅ |
| **Post-Compromise Security** | Double Ratchet (hash ratchet) | ✅ |
| **Post-Quantum Security** | PQXDH with ML-KEM | ✅ |
| **Zero-Knowledge Server** | Server only sees encrypted blobs | ✅ |

## References

- [Signal Protocol Specifications](https://signal.org/docs/)
- [Double Ratchet Algorithm](https://signal.org/docs/specifications/doubleratchet/)
- [X3DH Key Agreement](https://signal.org/docs/specifications/x3dh/)
- [PQXDH (Post-Quantum X3DH)](https://signal.org/docs/specifications/pqxdh/)
- [signalapp/libsignal Repository](https://github.com/signalapp/libsignal)
- [libcrux - Formally Verified Crypto](https://github.com/cryspen/libcrux)
- [getrandom WASM Support](https://docs.rs/getrandom/latest/getrandom/#webassembly-support)

## Changelog

- **2026-01-10**: Verified WASM compilation of libsignal-protocol, documented approach
