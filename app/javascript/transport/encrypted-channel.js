/**
 * EncryptedChannel - Reusable E2E Encrypted ActionCable Transport
 *
 * This class provides a unified interface for encrypted communication
 * over ActionCable channels. It handles:
 * - Signal Protocol session management (reuses existing sessions)
 * - Message encryption/decryption
 * - Channel subscription/lifecycle
 * - Reconnection with exponential backoff
 *
 * Usage:
 * ```javascript
 * const channel = new EncryptedChannel({
 *   channelName: 'PreviewChannel',
 *   hubId: '123',
 *   agentIndex: 0,
 *   signal: signalSession,
 *   onMessage: (msg) => console.log('Received:', msg),
 *   onStateChange: (state) => console.log('State:', state),
 * });
 *
 * await channel.connect();
 * await channel.send({ type: 'http_request', ... });
 * channel.disconnect();
 * ```
 */

import consumer from "channels/consumer";
import { SignalSession } from "signal";

/**
 * Channel connection states.
 */
export const ChannelState = {
  DISCONNECTED: "disconnected",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  RECONNECTING: "reconnecting",
  ERROR: "error",
};

/**
 * Default configuration.
 */
const DEFAULT_CONFIG = {
  reconnectMinDelay: 1000,
  reconnectMaxDelay: 30000,
  reconnectJitter: 0.2,
  maxReconnectAttempts: Infinity,
};

/**
 * EncryptedChannel - ActionCable channel with Signal Protocol encryption.
 */
export class EncryptedChannel {
  /**
   * Create a new encrypted channel.
   *
   * @param {Object} options - Channel configuration
   * @param {string} options.channelName - ActionCable channel name (e.g., 'PreviewChannel')
   * @param {string} options.hubId - Hub identifier
   * @param {number} [options.agentIndex] - Agent index (required for PreviewChannel)
   * @param {SignalSession} options.signal - Signal Protocol session
   * @param {Function} [options.onMessage] - Callback for decrypted messages
   * @param {Function} [options.onStateChange] - Callback for state changes
   * @param {Function} [options.onError] - Callback for errors
   * @param {Object} [options.config] - Override default configuration
   */
  constructor(options) {
    this.channelName = options.channelName;
    this.hubId = options.hubId;
    this.agentIndex = options.agentIndex;
    this.signal = options.signal;
    this.onMessage = options.onMessage || (() => {});
    this.onStateChange = options.onStateChange || (() => {});
    this.onError = options.onError || (() => {});
    this.config = { ...DEFAULT_CONFIG, ...options.config };

    this.subscription = null;
    this.state = ChannelState.DISCONNECTED;
    this.reconnectAttempt = 0;
    this.reconnectTimer = null;
    this.identity = null;
  }

  /**
   * Connect to the channel.
   *
   * @returns {Promise<void>} Resolves when connected
   */
  async connect() {
    if (this.state === ChannelState.CONNECTED) {
      return;
    }

    this.setState(ChannelState.CONNECTING);

    try {
      // Get our identity key for subscription
      this.identity = await this.signal.getIdentityKey();

      // Build subscription params based on channel type
      const params = this.buildSubscriptionParams();

      // Subscribe to ActionCable channel
      await this.subscribe(params);

      this.reconnectAttempt = 0;
      this.setState(ChannelState.CONNECTED);
    } catch (error) {
      console.error(`[EncryptedChannel] Connection failed:`, error);
      this.handleError(error);
    }
  }

  /**
   * Disconnect from the channel.
   */
  disconnect() {
    this.clearReconnectTimer();

    if (this.subscription) {
      this.subscription.unsubscribe();
      this.subscription = null;
    }

    this.setState(ChannelState.DISCONNECTED);
  }

  /**
   * Send an encrypted message.
   *
   * @param {Object} message - Message to encrypt and send
   * @returns {Promise<boolean>} True if sent successfully
   */
  async send(message) {
    if (this.state !== ChannelState.CONNECTED || !this.subscription) {
      console.warn("[EncryptedChannel] Cannot send - not connected");
      return false;
    }

    try {
      const envelope = await this.signal.encrypt(message);
      this.subscription.perform("relay", { envelope });
      return true;
    } catch (error) {
      console.error("[EncryptedChannel] Encryption failed:", error);
      this.onError({ type: "encryption_failed", error });
      return false;
    }
  }

  /**
   * Send an encrypted message to a specific recipient (agent -> browser routing).
   *
   * @param {Object} message - Message to encrypt and send
   * @param {string} recipientIdentity - Target browser's identity key
   * @returns {Promise<boolean>} True if sent successfully
   */
  async sendTo(message, recipientIdentity) {
    if (this.state !== ChannelState.CONNECTED || !this.subscription) {
      console.warn("[EncryptedChannel] Cannot send - not connected");
      return false;
    }

    try {
      const envelope = await this.signal.encrypt(message);
      this.subscription.perform("relay", {
        envelope,
        recipient_identity: recipientIdentity,
      });
      return true;
    } catch (error) {
      console.error("[EncryptedChannel] Encryption failed:", error);
      this.onError({ type: "encryption_failed", error });
      return false;
    }
  }

  /**
   * Get current connection state.
   *
   * @returns {string} Current state
   */
  getState() {
    return this.state;
  }

  /**
   * Check if connected.
   *
   * @returns {boolean} True if connected
   */
  isConnected() {
    return this.state === ChannelState.CONNECTED;
  }

  // === Private Methods ===

  /**
   * Build ActionCable subscription params based on channel type.
   */
  buildSubscriptionParams() {
    const params = {
      channel: this.channelName,
      hub_id: this.hubId,
      browser_identity: this.identity,
    };

    // PreviewChannel requires agent_index
    if (this.channelName === "PreviewChannel") {
      if (this.agentIndex === undefined || this.agentIndex === null) {
        throw new Error("PreviewChannel requires agentIndex");
      }
      params.agent_index = this.agentIndex;
    }

    return params;
  }

  /**
   * Subscribe to ActionCable channel.
   */
  subscribe(params) {
    return new Promise((resolve, reject) => {
      this.subscription = consumer.subscriptions.create(params, {
        connected: () => {
          console.log(`[EncryptedChannel] ${this.channelName} connected`);
          resolve();
        },
        disconnected: () => {
          console.log(`[EncryptedChannel] ${this.channelName} disconnected`);
          this.handleDisconnect();
        },
        rejected: () => {
          console.error(`[EncryptedChannel] ${this.channelName} subscription rejected`);
          reject(new Error("Subscription rejected"));
        },
        received: async (data) => {
          await this.handleReceived(data);
        },
      });
    });
  }

  /**
   * Handle received data from channel.
   */
  async handleReceived(data) {
    try {
      if (data.envelope) {
        // Decrypt Signal envelope
        const decrypted = await this.signal.decrypt(data.envelope);
        this.onMessage(decrypted);
      } else if (data.error) {
        this.onError({ type: "server_error", error: data.error });
      } else {
        // Pass through unencrypted messages (e.g., control messages)
        this.onMessage(data);
      }
    } catch (error) {
      console.error("[EncryptedChannel] Decryption failed:", error);
      this.onError({ type: "decryption_failed", error });
    }
  }

  /**
   * Handle disconnection - schedule reconnect.
   */
  handleDisconnect() {
    if (this.state === ChannelState.DISCONNECTED) {
      return; // Intentional disconnect
    }

    this.setState(ChannelState.RECONNECTING);
    this.scheduleReconnect();
  }

  /**
   * Handle connection error.
   */
  handleError(error) {
    this.setState(ChannelState.ERROR);
    this.onError({ type: "connection_failed", error });
    this.scheduleReconnect();
  }

  /**
   * Schedule a reconnection attempt.
   */
  scheduleReconnect() {
    if (this.reconnectAttempt >= this.config.maxReconnectAttempts) {
      console.error("[EncryptedChannel] Max reconnect attempts reached");
      this.onError({ type: "max_reconnects_reached" });
      return;
    }

    this.clearReconnectTimer();

    // Exponential backoff with jitter
    const baseDelay = Math.min(
      this.config.reconnectMinDelay * Math.pow(2, this.reconnectAttempt),
      this.config.reconnectMaxDelay
    );
    const jitter = baseDelay * this.config.reconnectJitter * (Math.random() * 2 - 1);
    const delay = Math.max(this.config.reconnectMinDelay, baseDelay + jitter);

    console.log(
      `[EncryptedChannel] Reconnecting in ${Math.round(delay)}ms (attempt ${this.reconnectAttempt + 1})`
    );

    this.reconnectTimer = setTimeout(async () => {
      this.reconnectAttempt++;
      await this.connect();
    }, delay);
  }

  /**
   * Clear any pending reconnect timer.
   */
  clearReconnectTimer() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  /**
   * Update state and notify listeners.
   */
  setState(state) {
    const prevState = this.state;
    this.state = state;
    if (prevState !== state) {
      this.onStateChange(state, prevState);
    }
  }
}

/**
 * PreviewChannel - Specialized EncryptedChannel for HTTP preview tunneling.
 *
 * Provides convenience methods for HTTP request/response handling.
 */
export class PreviewChannel extends EncryptedChannel {
  /**
   * Create a preview channel.
   *
   * @param {Object} options - Same as EncryptedChannel, plus:
   * @param {number} options.agentIndex - Agent index (required)
   */
  constructor(options) {
    super({
      ...options,
      channelName: "PreviewChannel",
    });

    this.pendingRequests = new Map();
    this.nextRequestId = 1;
  }

  /**
   * Send an HTTP request through the tunnel.
   *
   * @param {Object} request - HTTP request details
   * @param {string} request.method - HTTP method
   * @param {string} request.url - Request URL
   * @param {Object} [request.headers] - Request headers
   * @param {Uint8Array|string} [request.body] - Request body
   * @param {number} [timeout=30000] - Request timeout in ms
   * @returns {Promise<Object>} HTTP response
   */
  async fetch(request, timeout = 30000) {
    const requestId = this.nextRequestId++;

    return new Promise((resolve, reject) => {
      // Set up timeout
      const timer = setTimeout(() => {
        this.pendingRequests.delete(requestId);
        reject(new Error(`Request timeout: ${request.method} ${request.url}`));
      }, timeout);

      // Store pending request
      this.pendingRequests.set(requestId, {
        resolve,
        reject,
        timer,
      });

      // Send encrypted request
      this.send({
        type: "http_request",
        request_id: requestId,
        method: request.method,
        url: request.url,
        headers: request.headers || {},
        body: request.body ? this.encodeBody(request.body) : null,
      }).catch((error) => {
        this.pendingRequests.delete(requestId);
        clearTimeout(timer);
        reject(error);
      });
    });
  }

  /**
   * Override message handler to route HTTP responses.
   */
  async handleReceived(data) {
    try {
      if (data.envelope) {
        const decrypted = await this.signal.decrypt(data.envelope);

        // Check if this is an HTTP response
        if (decrypted.type === "http_response" && decrypted.request_id) {
          this.handleHttpResponse(decrypted);
        } else {
          this.onMessage(decrypted);
        }
      } else if (data.error) {
        this.onError({ type: "server_error", error: data.error });
      } else {
        this.onMessage(data);
      }
    } catch (error) {
      console.error("[PreviewChannel] Decryption failed:", error);
      this.onError({ type: "decryption_failed", error });
    }
  }

  /**
   * Handle HTTP response message.
   */
  handleHttpResponse(response) {
    const pending = this.pendingRequests.get(response.request_id);
    if (!pending) {
      console.warn("[PreviewChannel] Received response for unknown request:", response.request_id);
      return;
    }

    this.pendingRequests.delete(response.request_id);
    clearTimeout(pending.timer);

    pending.resolve({
      status: response.status,
      statusText: response.status_text,
      headers: response.headers,
      body: response.body ? this.decodeBody(response.body) : null,
    });
  }

  /**
   * Encode request body to base64.
   */
  encodeBody(body) {
    if (typeof body === "string") {
      return btoa(body);
    }
    if (body instanceof Uint8Array) {
      let binary = "";
      for (let i = 0; i < body.length; i++) {
        binary += String.fromCharCode(body[i]);
      }
      return btoa(binary);
    }
    return btoa(JSON.stringify(body));
  }

  /**
   * Decode response body from base64.
   */
  decodeBody(base64) {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }

  /**
   * Clean up pending requests on disconnect.
   */
  disconnect() {
    // Reject all pending requests
    for (const [, pending] of this.pendingRequests) {
      clearTimeout(pending.timer);
      pending.reject(new Error("Channel disconnected"));
    }
    this.pendingRequests.clear();

    super.disconnect();
  }
}

export default EncryptedChannel;
