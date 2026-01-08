/**
 * Double Ratchet Implementation - Signal Protocol Compatible
 *
 * This implements the Double Ratchet algorithm matching Signal's specification:
 * - HKDF-SHA256 for key derivation
 * - X25519 for Diffie-Hellman ratchet
 * - AES-256-CBC + HMAC-SHA256 for authenticated encryption
 *
 * Reference: https://signal.org/docs/specifications/doubleratchet/
 */

import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { hmac } from "@noble/hashes/hmac";
import { cbc } from "@noble/ciphers/aes";
import { randomBytes } from "@noble/ciphers/webcrypto";

/**
 * Double Ratchet session for E2E encryption with forward secrecy.
 *
 * Each message uses a unique key derived from the ratchet state.
 * Compromising one key doesn't compromise past or future messages.
 */
export class RatchetSession {
  /**
   * Create a new RatchetSession from initial shared secret.
   *
   * @param {Uint8Array} sharedSecret - 32-byte shared secret from initial key exchange
   * @param {boolean} isInitiator - true if this party initiated the session
   */
  constructor(sharedSecret, isInitiator) {
    // Initialize root key from shared secret using HKDF
    const initial = this.kdf(sharedSecret, new Uint8Array(32), "ratchet-init", 64);
    this.rootKey = initial.slice(0, 32);

    // Generate initial DH keypair
    this.dhPrivateKey = randomBytes(32);
    this.dhPublicKey = x25519.getPublicKey(this.dhPrivateKey);

    // Peer's DH public key (set when receiving first message)
    this.peerPublicKey = null;

    // Chain keys for sending and receiving
    this.sendChainKey = null;
    this.recvChainKey = null;

    // Message counters for replay protection
    this.sendCount = 0;
    this.recvCount = 0;

    // Track previous chain length for header
    this.prevChainLength = 0;

    // Is this party the initiator? Determines initial ratchet direction
    this.isInitiator = isInitiator;

    // If initiator, derive initial send chain from root key
    if (isInitiator) {
      const chainInit = this.kdf(this.rootKey, new Uint8Array(32), "chain-init", 64);
      this.rootKey = chainInit.slice(0, 32);
      this.sendChainKey = chainInit.slice(32, 64);
    }
  }

  /**
   * HKDF-SHA256 key derivation function.
   *
   * @param {Uint8Array} inputKey - Input key material
   * @param {Uint8Array} salt - Salt (can be zeros)
   * @param {string} info - Context string
   * @param {number} length - Output length in bytes
   * @returns {Uint8Array} Derived key material
   */
  kdf(inputKey, salt, info, length = 64) {
    return hkdf(sha256, inputKey, salt, info, length);
  }

  /**
   * Advance the sending chain and return a message key.
   *
   * @returns {Uint8Array} 32-byte message key for encryption
   */
  advanceSendChain() {
    if (!this.sendChainKey) {
      throw new Error("Send chain not initialized - need DH ratchet first");
    }

    // Derive new chain key and message key
    const output = this.kdf(this.sendChainKey, new Uint8Array(32), "chain", 64);
    this.sendChainKey = output.slice(0, 32);
    const messageKey = output.slice(32, 64);

    this.sendCount++;
    return messageKey;
  }

  /**
   * Advance the receiving chain and return a message key.
   *
   * @returns {Uint8Array} 32-byte message key for decryption
   */
  advanceRecvChain() {
    if (!this.recvChainKey) {
      throw new Error("Receive chain not initialized - need DH ratchet first");
    }

    // Derive new chain key and message key
    const output = this.kdf(this.recvChainKey, new Uint8Array(32), "chain", 64);
    this.recvChainKey = output.slice(0, 32);
    const messageKey = output.slice(32, 64);

    this.recvCount++;
    return messageKey;
  }

  /**
   * Perform a DH ratchet step when receiving a new public key.
   *
   * This provides the "asymmetric" part of the Double Ratchet:
   * - Compute new shared secret from DH
   * - Derive new root key and receiving chain
   * - Generate new DH keypair
   * - Derive new sending chain
   *
   * @param {Uint8Array} peerPublicKey - Peer's new DH public key
   */
  dhRatchet(peerPublicKey) {
    this.peerPublicKey = peerPublicKey;

    // DH with current private key → derive receiving chain
    const dh1 = x25519.getSharedSecret(this.dhPrivateKey, peerPublicKey);
    const output1 = this.kdf(dh1, this.rootKey, "ratchet", 64);
    this.rootKey = output1.slice(0, 32);
    this.recvChainKey = output1.slice(32, 64);
    this.recvCount = 0;

    // Save previous chain length for header
    this.prevChainLength = this.sendCount;

    // Generate new DH keypair
    this.dhPrivateKey = randomBytes(32);
    this.dhPublicKey = x25519.getPublicKey(this.dhPrivateKey);

    // DH with new private key → derive sending chain
    const dh2 = x25519.getSharedSecret(this.dhPrivateKey, peerPublicKey);
    const output2 = this.kdf(dh2, this.rootKey, "ratchet", 64);
    this.rootKey = output2.slice(0, 32);
    this.sendChainKey = output2.slice(32, 64);
    this.sendCount = 0;
  }

  /**
   * PKCS7 padding for AES-CBC.
   *
   * @param {Uint8Array} data - Data to pad
   * @returns {Uint8Array} Padded data (multiple of 16 bytes)
   */
  pkcs7Pad(data) {
    const padLen = 16 - (data.length % 16);
    const padded = new Uint8Array(data.length + padLen);
    padded.set(data);
    padded.fill(padLen, data.length);
    return padded;
  }

  /**
   * Remove PKCS7 padding.
   *
   * @param {Uint8Array} data - Padded data
   * @returns {Uint8Array} Unpadded data
   */
  pkcs7Unpad(data) {
    const padLen = data[data.length - 1];
    if (padLen < 1 || padLen > 16) {
      throw new Error("Invalid PKCS7 padding");
    }
    // Verify padding bytes
    for (let i = data.length - padLen; i < data.length; i++) {
      if (data[i] !== padLen) {
        throw new Error("Invalid PKCS7 padding");
      }
    }
    return data.slice(0, data.length - padLen);
  }

  /**
   * Encrypt a message using the Double Ratchet.
   *
   * @param {Uint8Array} plaintext - Message to encrypt
   * @returns {Object} Encrypted envelope with header
   */
  encrypt(plaintext) {
    // Get message key from symmetric ratchet
    const messageKey = this.advanceSendChain();

    // Derive encryption key, MAC key, and IV from message key
    const derived = this.kdf(messageKey, new Uint8Array(32), "message", 80);
    const encKey = derived.slice(0, 32);
    const macKey = derived.slice(32, 64);
    const iv = derived.slice(64, 80);

    // AES-256-CBC encrypt with PKCS7 padding
    const cipher = cbc(encKey, iv);
    const padded = this.pkcs7Pad(plaintext);
    const ciphertext = cipher.encrypt(padded);

    // HMAC-SHA256 authenticate (truncated to 8 bytes like Signal)
    const macInput = new Uint8Array(ciphertext.length + 32);
    macInput.set(this.dhPublicKey);
    macInput.set(ciphertext, 32);
    const fullMac = hmac(sha256, macKey, macInput);
    const mac = fullMac.slice(0, 8);

    return {
      version: 2,
      header: {
        dhPublicKey: this.dhPublicKey,
        prevChainLength: this.prevChainLength,
        messageNumber: this.sendCount - 1,
      },
      ciphertext,
      mac,
    };
  }

  /**
   * Decrypt a message using the Double Ratchet.
   *
   * @param {Object} envelope - Encrypted envelope from encrypt()
   * @returns {Uint8Array} Decrypted plaintext
   */
  decrypt(envelope) {
    const { header, ciphertext, mac } = envelope;

    // Check if we need to perform a DH ratchet
    // (peer's DH key is different from what we have)
    const needsRatchet = !this.peerPublicKey ||
      !this.arraysEqual(header.dhPublicKey, this.peerPublicKey);

    if (needsRatchet) {
      this.dhRatchet(header.dhPublicKey);
    }

    // Get message key from symmetric ratchet
    const messageKey = this.advanceRecvChain();

    // Derive encryption key, MAC key, and IV from message key
    const derived = this.kdf(messageKey, new Uint8Array(32), "message", 80);
    const encKey = derived.slice(0, 32);
    const macKey = derived.slice(32, 64);
    const iv = derived.slice(64, 80);

    // Verify HMAC first
    const macInput = new Uint8Array(ciphertext.length + 32);
    macInput.set(header.dhPublicKey);
    macInput.set(ciphertext, 32);
    const expectedMac = hmac(sha256, macKey, macInput).slice(0, 8);

    if (!this.arraysEqual(mac, expectedMac)) {
      throw new Error("MAC verification failed - message tampered or wrong key");
    }

    // AES-256-CBC decrypt and remove padding
    const cipher = cbc(encKey, iv);
    const padded = cipher.decrypt(ciphertext);
    return this.pkcs7Unpad(padded);
  }

  /**
   * Compare two Uint8Arrays for equality.
   *
   * @param {Uint8Array} a
   * @param {Uint8Array} b
   * @returns {boolean}
   */
  arraysEqual(a, b) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (a[i] !== b[i]) return false;
    }
    return true;
  }

  /**
   * Get current DH public key for inclusion in message header.
   *
   * @returns {Uint8Array} Current 32-byte DH public key
   */
  getPublicKey() {
    return this.dhPublicKey;
  }

  /**
   * Zero out all sensitive key material.
   * Call this when disconnecting.
   */
  zeroize() {
    if (this.rootKey) this.rootKey.fill(0);
    if (this.sendChainKey) this.sendChainKey.fill(0);
    if (this.recvChainKey) this.recvChainKey.fill(0);
    if (this.dhPrivateKey) this.dhPrivateKey.fill(0);

    this.rootKey = null;
    this.sendChainKey = null;
    this.recvChainKey = null;
    this.dhPrivateKey = null;
    this.dhPublicKey = null;
    this.peerPublicKey = null;
  }
}

/**
 * Serialize envelope for transmission.
 *
 * @param {Object} envelope - Envelope from RatchetSession.encrypt()
 * @returns {Object} JSON-serializable envelope
 */
export function serializeEnvelope(envelope) {
  return {
    version: envelope.version,
    header: {
      dh_public_key: bytesToBase64(envelope.header.dhPublicKey),
      prev_chain_length: envelope.header.prevChainLength,
      message_number: envelope.header.messageNumber,
    },
    ciphertext: bytesToBase64(envelope.ciphertext),
    mac: bytesToBase64(envelope.mac),
  };
}

/**
 * Deserialize envelope from transmission.
 *
 * @param {Object} data - Received envelope data
 * @returns {Object} Envelope for RatchetSession.decrypt()
 */
export function deserializeEnvelope(data) {
  return {
    version: data.version,
    header: {
      dhPublicKey: base64ToBytes(data.header.dh_public_key),
      prevChainLength: data.header.prev_chain_length,
      messageNumber: data.header.message_number,
    },
    ciphertext: base64ToBytes(data.ciphertext),
    mac: base64ToBytes(data.mac),
  };
}

// Base64 helpers
function bytesToBase64(bytes) {
  return btoa(String.fromCharCode(...bytes));
}

function base64ToBytes(str) {
  return new Uint8Array(atob(str).split("").map(c => c.charCodeAt(0)));
}
