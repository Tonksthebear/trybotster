/**
 * Channel - WebSocket channel with optional reliable delivery.
 *
 * Mirrors the Rust ActionCableChannel architecture:
 * - Optional encryption (via SignalSession)
 * - Optional reliable delivery (seq numbers, ACKs, retransmit, reorder buffer)
 * - Clean API: send()/onMessage callback
 *
 * Usage:
 *   const channel = new Channel({
 *     subscription: consumer.subscriptions.create(...),
 *     session: signalSession,     // optional: for E2E encryption
 *     reliable: true,             // optional: for guaranteed delivery
 *     onMessage: (msg) => { ... }
 *   });
 *
 *   await channel.send(message);
 */

import { ReliableSender, ReliableReceiver } from "channels/reliable_channel";

// Compression marker bytes (must match CLI's compression.rs)
const MARKER_UNCOMPRESSED = 0x00;
const MARKER_GZIP = 0x1f;

// How often to check for heartbeat ACKs (should match CLI's HEALTH_CHECK_INTERVAL_SECS)
const MAINTENANCE_INTERVAL_MS = 5000;

/**
 * Strip compression marker and decompress if needed.
 * The CLI adds a marker byte (0x00 = uncompressed, 0x1f = gzip) to all messages.
 *
 * @param {string} data - Decrypted string (may have marker prefix)
 * @returns {Promise<Object>} - Parsed JSON object
 */
async function decompressMessage(data) {
  // Convert string to bytes to check marker
  const bytes = new TextEncoder().encode(data);

  if (bytes.length === 0) {
    throw new Error("Empty message");
  }

  const marker = bytes[0];

  if (marker === MARKER_UNCOMPRESSED) {
    // Strip marker, parse rest as JSON
    const jsonBytes = bytes.slice(1);
    const jsonString = new TextDecoder().decode(jsonBytes);
    return JSON.parse(jsonString);
  } else if (marker === MARKER_GZIP) {
    // Gzip compressed - strip marker and decompress
    const compressedBytes = bytes.slice(1);
    const stream = new Blob([compressedBytes])
      .stream()
      .pipeThrough(new DecompressionStream("gzip"));
    const decompressed = await new Response(stream).text();
    return JSON.parse(decompressed);
  } else {
    // No marker - treat as raw JSON (backwards compatibility)
    return JSON.parse(data);
  }
}

/**
 * Channel with optional encryption and reliability.
 */
export class Channel {
  /**
   * Create a new channel.
   *
   * @param {Object} options
   * @param {Object} options.subscription - ActionCable subscription object
   * @param {Object} options.session - SignalSession for encryption (optional)
   * @param {boolean} options.reliable - Enable reliable delivery (default: false)
   * @param {Function} options.onMessage - Callback for received messages
   * @param {Function} options.onConnect - Callback when channel connects
   * @param {Function} options.onDisconnect - Callback when channel disconnects
   */
  constructor(options = {}) {
    this.subscription = options.subscription;
    this.session = options.session || null;
    this.reliable = options.reliable || false;
    this.onMessage = options.onMessage || (() => {});
    this.onConnect = options.onConnect || (() => {});
    this.onDisconnect = options.onDisconnect || (() => {});
    this.onError = options.onError || (() => {});

    // Track consecutive decryption failures to detect stale sessions
    this.decryptionFailureCount = 0;
    this.maxDecryptionFailures = 3; // After 3 failures, emit session_invalid error

    // Reliable delivery components (only if enabled)
    this.sender = this.reliable
      ? new ReliableSender({
          retransmitTimeout: 3000,
          onSend: (msg) => this._rawSend(msg),
          onMessageFailed: (seq, payload) => {
            console.error(`[Channel] Message seq=${seq} permanently failed after max retransmits`);
          },
        })
      : null;

    this.receiver = this.reliable
      ? new ReliableReceiver({
          onDeliver: (payload) => this.onMessage(payload),
          onAck: (ack) => this._rawSend(ack),
          onReset: () => {
            // When receiver detects peer session reset, also reset sender
            console.debug("[Channel] Peer session reset detected, resetting sender");
            this.sender?.reset();
          },
        })
      : null;

    // Connection state
    this.connected = false;

    // Maintenance interval for heartbeat ACKs (only if reliable)
    this.maintenanceTimer = null;
    if (this.reliable) {
      this._startMaintenance();
    }
  }

  /**
   * Start the maintenance interval for heartbeat ACKs.
   * @private
   */
  _startMaintenance() {
    if (this.maintenanceTimer) return;

    this.maintenanceTimer = setInterval(() => {
      // Send heartbeat ACK if receiver hasn't ACK'd recently
      if (this.receiver && this.receiver.shouldSendAckHeartbeat()) {
        console.debug("[Channel] Sending heartbeat ACK");
        const ack = this.receiver.generateAck();
        this._rawSend(ack);
      }
    }, MAINTENANCE_INTERVAL_MS);
  }

  /**
   * Stop the maintenance interval.
   * @private
   */
  _stopMaintenance() {
    if (this.maintenanceTimer) {
      clearInterval(this.maintenanceTimer);
      this.maintenanceTimer = null;
    }
  }

  /**
   * Create a channel with builder-style configuration.
   *
   * @param {Object} subscription - ActionCable subscription
   * @returns {ChannelBuilder}
   */
  static builder(subscription) {
    return new ChannelBuilder(subscription);
  }

  /**
   * Send a message through the channel.
   *
   * If reliable delivery is enabled, the message is wrapped with a sequence
   * number and will be retransmitted until acknowledged.
   *
   * If encryption is enabled, the message is encrypted before sending.
   *
   * @param {Object} message - The message to send
   * @returns {Promise<boolean>} - True if send succeeded
   */
  async send(message) {
    if (this.reliable && this.sender) {
      // Wrap in reliable envelope (sender will call _rawSend)
      this.sender.send(message);
      return true;
    } else {
      // Direct send (non-reliable)
      return await this._rawSend(message);
    }
  }

  /**
   * Process a received message from ActionCable.
   *
   * Call this from the subscription's received callback.
   * Handles decryption and reliable delivery processing.
   *
   * @param {Object} data - Raw data from ActionCable
   */
  async receive(data) {
    let decrypted = data;

    // Decrypt if we have a session and data has envelope
    if (this.session && data.envelope) {
      try {
        decrypted = await this.session.decrypt(data.envelope);
        // Reset failure count on successful decryption
        this.decryptionFailureCount = 0;
      } catch (error) {
        this.decryptionFailureCount++;
        console.error(`[Channel] Decryption failed (${this.decryptionFailureCount}/${this.maxDecryptionFailures}):`, error);

        // After repeated failures, the session is likely invalid (CLI restarted, keys changed)
        if (this.decryptionFailureCount >= this.maxDecryptionFailures) {
          console.error("[Channel] Session appears invalid - CLI may have restarted. Re-scan QR code required.");
          this.onError({
            type: "session_invalid",
            message: "Encryption session expired. Please re-scan the QR code to reconnect.",
            failureCount: this.decryptionFailureCount,
          });
        }
        return;
      }
    }

    // Handle case where decrypt returns a string (needs decompression + JSON parsing)
    // The CLI prepends a compression marker byte (0x00 = uncompressed, 0x1f = gzip)
    if (typeof decrypted === "string") {
      try {
        decrypted = await decompressMessage(decrypted);
      } catch (error) {
        console.error("[Channel] Failed to decompress/parse decrypted message:", error);
        return;
      }
    }

    // Process through reliable layer if enabled
    if (this.reliable && this.receiver) {
      if (decrypted.type === "data" && decrypted.seq != null) {
        // Reliable data message - receiver.receive() is async (decompression may be needed)
        await this.receiver.receive(decrypted.seq, decrypted.payload);
      } else if (decrypted.type === "ack" && decrypted.ranges) {
        // ACK message - update sender's pending set
        if (this.sender) {
          this.sender.processAck(decrypted.ranges);
        }
      } else {
        // Non-reliable message (backwards compat or control messages)
        this.onMessage(decrypted);
      }
    } else {
      // Non-reliable: deliver directly
      this.onMessage(decrypted);
    }
  }

  /**
   * Mark channel as connected.
   * Called when ActionCable subscription confirms.
   * Resumes retransmission if paused.
   */
  markConnected() {
    this.connected = true;
    this.sender?.resume();
    this.onConnect();
  }

  /**
   * Mark channel as disconnected.
   * Called when ActionCable subscription disconnects.
   * Pauses retransmission to avoid wasted effort.
   */
  markDisconnected() {
    this.connected = false;
    this.sender?.pause();
    this.onDisconnect();
  }

  /**
   * Clean up timers and resources.
   */
  destroy() {
    this._stopMaintenance();
    if (this.sender) {
      this.sender.destroy();
      this.sender = null;
    }
    if (this.receiver) {
      this.receiver.destroy();
      this.receiver = null;
    }
    this.connected = false;
  }

  /**
   * Internal: Send raw message through ActionCable.
   * Encrypts if session is available.
   */
  async _rawSend(message) {
    if (!this.subscription) {
      console.warn("[Channel] Cannot send - no subscription");
      return false;
    }

    try {
      if (this.session) {
        // Encrypt before sending
        const envelope = await this.session.encrypt(message);
        this.subscription.perform("relay", { envelope });
      } else {
        // Unencrypted
        this.subscription.perform("relay", { data: message });
      }
      return true;
    } catch (error) {
      console.error("[Channel] Send failed:", error);
      return false;
    }
  }
}

/**
 * Builder for Channel with fluent API.
 */
export class ChannelBuilder {
  constructor(subscription) {
    this._subscription = subscription;
    this._session = null;
    this._reliable = false;
    this._onMessage = () => {};
    this._onConnect = () => {};
    this._onDisconnect = () => {};
    this._onError = () => {};
  }

  /**
   * Set the SignalSession for E2E encryption.
   */
  session(session) {
    this._session = session;
    return this;
  }

  /**
   * Enable reliable delivery (TCP-like guarantees).
   */
  reliable(enable = true) {
    this._reliable = enable;
    return this;
  }

  /**
   * Set the message callback.
   */
  onMessage(callback) {
    this._onMessage = callback;
    return this;
  }

  /**
   * Set the connect callback.
   */
  onConnect(callback) {
    this._onConnect = callback;
    return this;
  }

  /**
   * Set the disconnect callback.
   */
  onDisconnect(callback) {
    this._onDisconnect = callback;
    return this;
  }

  /**
   * Set the error callback.
   */
  onError(callback) {
    this._onError = callback;
    return this;
  }

  /**
   * Build the channel.
   */
  build() {
    return new Channel({
      subscription: this._subscription,
      session: this._session,
      reliable: this._reliable,
      onMessage: this._onMessage,
      onConnect: this._onConnect,
      onDisconnect: this._onDisconnect,
      onError: this._onError,
    });
  }
}
