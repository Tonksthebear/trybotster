/**
 * Olm E2E Encryption - vodozemac WASM wrapper
 *
 * This module provides E2E encryption using vodozemac's Olm implementation,
 * the same battle-tested, NCC-audited cryptography used by Matrix.
 *
 * Protocol Flow:
 * 1. Browser scans QR code with CLI's keys (ed25519, curve25519, one_time_key)
 * 2. Browser creates outbound Olm session using CLI's keys
 * 3. Browser sends first message as PreKey message (establishes session)
 * 4. CLI creates inbound session from PreKey message
 * 5. Both sides can now encrypt/decrypt with the session
 */

// Import the WASM JS bindings (pinned in importmap)
import * as vodozemacWasm from "wasm/vodozemac_wasm";

let wasmInitialized = false;

/**
 * Initialize the vodozemac WASM module.
 * Must be called before using any Olm functions.
 *
 * @returns {Promise<void>}
 */
export async function initOlm() {
  if (wasmInitialized) {
    return;
  }

  try {
    // Load the WASM binary from public URL
    const wasmUrl = "/wasm/vodozemac_wasm_bg.wasm";
    await vodozemacWasm.default(wasmUrl);

    // Call the init function if available
    if (vodozemacWasm.init) {
      vodozemacWasm.init();
    }

    wasmInitialized = true;
    console.log("vodozemac WASM initialized successfully");
  } catch (error) {
    console.error("Failed to initialize vodozemac WASM:", error);
    throw error;
  }
}

/**
 * Olm session wrapper for browser-side encryption.
 *
 * The browser creates an outbound session using the CLI's keys from the QR code.
 * The first message sent is always a PreKey message that establishes the session.
 */
export class OlmSession {
  /**
   * Create a new OlmSession for communicating with the CLI.
   *
   * @param {string} cliCurve25519 - CLI's Curve25519 identity key (base64)
   * @param {string} cliOneTimeKey - CLI's one-time key (base64)
   */
  constructor(cliCurve25519, cliOneTimeKey) {
    if (!wasmInitialized) {
      throw new Error("Olm WASM not initialized - call initOlm() first");
    }

    // Create a browser account (for identity)
    this.account = new vodozemacWasm.Account();
    this.identityKeys = this.account.identity_keys();

    // Create outbound session to CLI using CLI's keys from QR code
    this.session = this.account.create_outbound_session(
      cliCurve25519,
      cliOneTimeKey
    );

    // Store CLI's identity key for envelope construction
    this.cliCurve25519 = cliCurve25519;

    console.log("Created outbound Olm session to CLI");
  }

  /**
   * Get browser's Curve25519 identity key (base64).
   *
   * @returns {string} Base64-encoded Curve25519 public key
   */
  getCurve25519Key() {
    return this.identityKeys.curve25519;
  }

  /**
   * Get browser's Ed25519 signing key (base64).
   *
   * @returns {string} Base64-encoded Ed25519 public key
   */
  getEd25519Key() {
    return this.identityKeys.ed25519;
  }

  /**
   * Encrypt a message for the CLI.
   *
   * @param {Object} message - Message object to encrypt (will be JSON serialized)
   * @returns {Object} OlmEnvelope ready for transmission
   */
  encrypt(message) {
    const plaintext = JSON.stringify(message);
    const encrypted = this.session.encrypt(plaintext);

    return {
      version: 3, // Olm protocol version
      message_type: encrypted.message_type, // 0 = PreKey, 1 = Normal
      ciphertext: encrypted.ciphertext,
      sender_key: this.getCurve25519Key(),
    };
  }

  /**
   * Decrypt a message from the CLI.
   *
   * @param {Object} envelope - OlmEnvelope received from CLI
   * @returns {Object} Decrypted and parsed message object
   */
  decrypt(envelope) {
    // Create EncryptedMessage from envelope
    const encrypted = new vodozemacWasm.EncryptedMessage(
      envelope.message_type,
      envelope.ciphertext
    );

    // Decrypt
    const plaintext = this.session.decrypt(encrypted);

    // Parse JSON
    return JSON.parse(plaintext);
  }

  /**
   * Get the session ID for debugging.
   *
   * @returns {string} Session ID
   */
  getSessionId() {
    return this.session.session_id();
  }

  /**
   * Pickle (serialize) the session for storage.
   *
   * @returns {Object} Pickled session data
   */
  pickle() {
    return {
      account: this.account.pickle(),
      session: this.session.pickle(),
      cliCurve25519: this.cliCurve25519,
    };
  }

  /**
   * Restore a session from pickled data.
   *
   * @param {Object} pickled - Pickled session data from pickle()
   * @returns {OlmSession} Restored session
   */
  static fromPickle(pickled) {
    if (!wasmInitialized) {
      throw new Error("Olm WASM not initialized - call initOlm() first");
    }

    const instance = Object.create(OlmSession.prototype);
    instance.account = vodozemacWasm.Account.from_pickle(pickled.account);
    instance.session = vodozemacWasm.Session.from_pickle(pickled.session);
    instance.identityKeys = instance.account.identity_keys();
    instance.cliCurve25519 = pickled.cliCurve25519;

    return instance;
  }

  /**
   * Clean up WASM resources.
   * Call this when disconnecting.
   */
  free() {
    if (this.session) {
      this.session.free();
      this.session = null;
    }
    if (this.identityKeys) {
      this.identityKeys.free();
      this.identityKeys = null;
    }
    if (this.account) {
      this.account.free();
      this.account = null;
    }
  }
}

/**
 * Serialize an OlmEnvelope for ActionCable transmission.
 *
 * @param {Object} envelope - Envelope from OlmSession.encrypt()
 * @returns {Object} JSON-serializable envelope
 */
export function serializeEnvelope(envelope) {
  return {
    version: envelope.version,
    message_type: envelope.message_type,
    ciphertext: envelope.ciphertext,
    sender_key: envelope.sender_key,
  };
}

/**
 * Deserialize an OlmEnvelope from ActionCable.
 *
 * @param {Object} data - Received envelope data
 * @returns {Object} Envelope for OlmSession.decrypt()
 */
export function deserializeEnvelope(data) {
  return {
    version: data.version,
    message_type: data.message_type,
    ciphertext: data.ciphertext,
    sender_key: data.sender_key,
  };
}
