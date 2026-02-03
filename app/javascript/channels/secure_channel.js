/**
 * SecureChannel - Abstracts all encryption/channel mechanics.
 *
 * Controllers don't need to know about Signal, WASM, Channel wrappers,
 * or ActionCable subscription mechanics. This module provides:
 *
 *   - loadSession(hubId, { fromFragment }) — loads WASM, creates/loads Signal session
 *   - open(options) — opens a secure E2E encrypted ActionCable channel
 *   - getHubIdFromPath — re-exported from signal
 *
 * WASM URLs are read from <meta> tags in the layout <head>, set once for
 * all pages. This eliminates the need to pass them as Stimulus values.
 *
 * Usage:
 *   import { loadSession, open, getHubIdFromPath } from "channels/secure_channel";
 *
 *   const hubId = getHubIdFromPath();
 *   const session = await loadSession(hubId, { fromFragment: true });
 *
 *   const handle = await open({
 *     channel: "HubChannel",
 *     params: { hub_id: hubId, browser_identity: identityKey },
 *     session,
 *     reliable: true,
 *     onMessage: (msg) => console.log(msg),
 *     onConnect: () => console.log("connected"),
 *     onDisconnect: () => console.log("disconnected"),
 *     onError: (err) => console.error(err),
 *   });
 *
 *   await handle.send({ type: "hello" });
 *   handle.close();
 */

import consumer from "channels/consumer";
import { Channel } from "channels/channel";
import {
  ensureSignalReady,
  SignalSession,
  parseBundleFromFragment,
  getHubIdFromPath,
} from "signal";

// ---------------------------------------------------------------------------
// WASM configuration from <meta> tags
// ---------------------------------------------------------------------------

function getSignalConfig() {
  return {
    workerUrl: document.querySelector('meta[name="signal-worker-url"]')
      ?.content,
    cryptoWorkerUrl: document.querySelector('meta[name="signal-crypto-worker-url"]')
      ?.content,
    wasmJsUrl: document.querySelector('meta[name="signal-wasm-js-url"]')
      ?.content,
    wasmBinaryUrl: document.querySelector('meta[name="signal-wasm-binary-url"]')
      ?.content,
  };
}

let _wasmReady = false;

async function ensureWasm() {
  if (_wasmReady) return;
  const { workerUrl, cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl } = getSignalConfig();
  await ensureSignalReady(workerUrl, cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl);
  _wasmReady = true;
}

// ---------------------------------------------------------------------------
// Session loading
// ---------------------------------------------------------------------------

/**
 * Load or create a Signal session for the given hub.
 *
 * Initializes the Signal WASM worker if it hasn't been loaded yet (idempotent).
 *
 * When `fromFragment` is true, checks the URL hash for a QR-code-scanned
 * PreKeyBundle. If found, creates a fresh session and strips the fragment
 * from the URL. Otherwise falls back to loading from IndexedDB.
 *
 * @param {string} hubId - Hub identifier
 * @param {Object} options
 * @param {boolean} options.fromFragment - Check URL fragment for PreKeyBundle
 * @returns {Promise<SignalSession|null>} - Session, or null if none available
 */
export async function loadSession(hubId, { fromFragment = false } = {}) {
  await ensureWasm();

  if (fromFragment) {
    const bundle = parseBundleFromFragment();
    if (bundle) {
      const session = await SignalSession.create(bundle, hubId);
      // Strip the fragment so the bundle isn't reprocessed on reload
      history.replaceState(null, "", location.pathname + location.search);
      return session;
    }
  }

  return await SignalSession.load(hubId);
}

// ---------------------------------------------------------------------------
// Channel opening
// ---------------------------------------------------------------------------

/**
 * Open a secure, E2E encrypted ActionCable channel.
 *
 * Creates an ActionCable subscription, wraps it with a Channel (handling
 * encryption + optional reliable delivery), and resolves with a
 * SecureChannelHandle on first successful connection.
 *
 * Rejects if the subscription is rejected by the server.
 *
 * @param {Object} options
 * @param {string} options.channel - ActionCable channel class name
 * @param {Object} options.params - Additional subscription params
 * @param {SignalSession} options.session - Signal session for encryption
 * @param {boolean} options.reliable - Enable reliable delivery (default: true)
 * @param {Function} options.onMessage - Callback for decrypted messages
 * @param {Function} options.onConnect - Callback on (re)connect
 * @param {Function} options.onDisconnect - Callback on disconnect
 * @param {Function} options.onError - Callback on channel error
 * @returns {Promise<SecureChannelHandle>}
 */
export function open(options) {
  const {
    channel: channelName,
    params = {},
    session,
    reliable = true,
    onMessage = () => {},
    onConnect = () => {},
    onDisconnect = () => {},
    onError = () => {},
  } = options;

  return new Promise((resolve, reject) => {
    let wrapped = null;
    let resolved = false;

    const subscription = consumer.subscriptions.create(
      { channel: channelName, ...params },
      {
        connected: () => {
          if (wrapped) {
            // Reconnect: resume without resetting
            // Reliable channel handles retransmission of pending messages
            wrapped.markConnected(false);
          } else {
            // First connect: create Channel
            wrapped = Channel.builder(subscription)
              .session(session)
              .reliable(reliable)
              .onMessage(onMessage)
              .onConnect(onConnect)
              .onDisconnect(onDisconnect)
              .onError(onError)
              .build();

            wrapped.markConnected();
          }

          if (!resolved) {
            resolved = true;
            resolve(new SecureChannelHandle(subscription, wrapped, session));
          }
        },

        disconnected: () => {
          // Don't destroy the Channel - just mark disconnected.
          // This preserves reliable channel state (seq numbers) and
          // allows proper resumption when ActionCable reconnects.
          // Destroying would reset seq numbers but not Signal counters,
          // causing decryption failures on reconnect.
          if (wrapped) {
            wrapped.markDisconnected();
          }
          onDisconnect();
        },

        rejected: () => {
          reject(new Error(`${channelName} rejected`));
        },

        received: async (data) => {
          // Handle SenderKey distribution (sent unencrypted by server)
          if (data.sender_key_distribution) {
            await session?.processSenderKeyDistribution(
              data.sender_key_distribution,
            );
            return;
          }

          // Handle server-side errors
          if (data.error) {
            onError({ type: "server_error", message: data.error });
            return;
          }

          // Everything else goes through the Channel for decryption + reliability
          if (wrapped) await wrapped.receive(data);
        },
      },
    );
  });
}

// ---------------------------------------------------------------------------
// SecureChannelHandle
// ---------------------------------------------------------------------------

class SecureChannelHandle {
  #subscription;
  #channel;
  #session;

  constructor(subscription, channel, session) {
    this.#subscription = subscription;
    this.#channel = channel;
    this.#session = session;
  }

  /**
   * Send an encrypted message through the channel.
   * @param {Object} message - JSON-serializable message
   * @returns {Promise<boolean>} - True if send succeeded
   */
  async send(message) {
    return this.#channel?.send(message) ?? false;
  }

  /**
   * Close the channel and unsubscribe from ActionCable.
   */
  close() {
    this.#channel?.destroy();
    this.#channel = null;
    this.#subscription?.unsubscribe();
    this.#subscription = null;
  }

  /** The Signal session backing this channel. */
  get session() {
    return this.#session;
  }

  /** Whether the channel is currently open. */
  get isOpen() {
    return this.#channel !== null;
  }
}

// Re-export for convenience
export { getHubIdFromPath };
