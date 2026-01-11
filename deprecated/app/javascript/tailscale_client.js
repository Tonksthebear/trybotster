/**
 * Tailscale integration for browser-to-CLI connectivity via tsconnect WASM.
 *
 * This module wraps the tsconnect WASM library to provide:
 * - Connection to Headscale control server
 * - Authentication via pre-auth key (from URL fragment)
 * - SSH tunnel to CLI for terminal access
 *
 * Security:
 * - Pre-auth key is in URL fragment (#key=xxx), never sent to server
 * - WireGuard encrypts all traffic end-to-end
 * - Per-user namespace isolation at Headscale infrastructure level
 *
 * API (tsconnect exports):
 * - createIPN(config) - creates IPN instance, loads WASM
 * - runSSHSession(ipn, hostname, user, termConfig) - SSH to peer
 */

// Import from the bundled tsconnect module
let createIPN, runSSHSession;
let wasmInitialized = false;
let initPromise = null;

const WASM_URL = "/wasm/tsconnect.wasm";
const JS_URL = "/wasm/tsconnect.js";

/**
 * Connection state for the Tailscale session.
 */
export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING_WASM: "loading_wasm",
  CONNECTING: "connecting",
  NEEDS_LOGIN: "needs_login",
  AUTHENTICATING: "authenticating",
  STARTING: "starting",
  CONNECTED: "connected",
  ERROR: "error",
};

/**
 * Map tsconnect internal states to our ConnectionState.
 */
const STATE_MAP = {
  NoState: ConnectionState.CONNECTING,
  NeedsLogin: ConnectionState.NEEDS_LOGIN,
  NeedsMachineAuth: ConnectionState.AUTHENTICATING,
  Stopped: ConnectionState.DISCONNECTED,
  Starting: ConnectionState.STARTING,
  Running: ConnectionState.CONNECTED,
};

/**
 * IndexedDB-based state storage for Tailscale IPN.
 *
 * The tsconnect WASM module uses this to persist connection state
 * across page reloads (session keys, preferences, etc).
 */
class IPNStateStorage {
  constructor(hubIdentifier) {
    this.hubIdentifier = hubIdentifier;
    this.dbName = "botster_tailscale_state";
    this.storeName = "ipn_state";
    this.db = null;
    this.cache = new Map();
  }

  async init() {
    return new Promise((resolve, reject) => {
      const request = indexedDB.open(this.dbName, 1);

      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        this.db = request.result;
        resolve();
      };

      request.onupgradeneeded = (event) => {
        const db = event.target.result;
        if (!db.objectStoreNames.contains(this.storeName)) {
          db.createObjectStore(this.storeName);
        }
      };
    });
  }

  // Called by tsconnect WASM - returns hex-encoded state
  getState(key) {
    const fullKey = `${this.hubIdentifier}:${key}`;

    // Return cached value synchronously (required by WASM)
    if (this.cache.has(fullKey)) {
      return this.cache.get(fullKey);
    }

    // Fall back to synchronous localStorage if IndexedDB not ready
    // (WASM calls are synchronous, can't await)
    try {
      return localStorage.getItem(`ts_state_${fullKey}`) || "";
    } catch {
      return "";
    }
  }

  // Called by tsconnect WASM - stores hex-encoded state
  setState(key, value) {
    const fullKey = `${this.hubIdentifier}:${key}`;
    this.cache.set(fullKey, value);

    // Persist to localStorage (synchronous fallback)
    try {
      localStorage.setItem(`ts_state_${fullKey}`, value);
    } catch (e) {
      console.warn("Failed to persist Tailscale state:", e);
    }

    // Also persist to IndexedDB async (more storage)
    if (this.db) {
      const tx = this.db.transaction(this.storeName, "readwrite");
      tx.objectStore(this.storeName).put(value, fullKey);
    }
  }

  // Pre-load cached state from storage
  async preload() {
    // Load from localStorage first (sync)
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key.startsWith(`ts_state_${this.hubIdentifier}:`)) {
        const shortKey = key.replace("ts_state_", "");
        this.cache.set(shortKey, localStorage.getItem(key));
      }
    }

    // Then load from IndexedDB (async, may override)
    if (this.db) {
      const tx = this.db.transaction(this.storeName, "readonly");
      const store = tx.objectStore(this.storeName);

      return new Promise((resolve) => {
        const request = store.openCursor();
        request.onsuccess = (event) => {
          const cursor = event.target.result;
          if (cursor) {
            if (cursor.key.startsWith(`${this.hubIdentifier}:`)) {
              this.cache.set(cursor.key, cursor.value);
            }
            cursor.continue();
          } else {
            resolve();
          }
        };
        request.onerror = () => resolve();
      });
    }
  }

  // Clear all state for this hub
  async clear() {
    // Clear cache
    for (const key of this.cache.keys()) {
      if (key.startsWith(`${this.hubIdentifier}:`)) {
        this.cache.delete(key);
      }
    }

    // Clear localStorage
    const keysToRemove = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key.startsWith(`ts_state_${this.hubIdentifier}:`)) {
        keysToRemove.push(key);
      }
    }
    keysToRemove.forEach((k) => localStorage.removeItem(k));

    // Clear IndexedDB
    if (this.db) {
      const tx = this.db.transaction(this.storeName, "readwrite");
      const store = tx.objectStore(this.storeName);
      const request = store.openCursor();
      request.onsuccess = (event) => {
        const cursor = event.target.result;
        if (cursor) {
          if (cursor.key.startsWith(`${this.hubIdentifier}:`)) {
            cursor.delete();
          }
          cursor.continue();
        }
      };
    }
  }
}

/**
 * Initialize the tsconnect WASM module.
 *
 * Must be called before any other Tailscale functions.
 * Safe to call multiple times (will only initialize once).
 *
 * @returns {Promise<void>}
 */
export async function initTailscale() {
  if (wasmInitialized) {
    return;
  }

  // Dedupe concurrent init calls
  if (initPromise) {
    return initPromise;
  }

  initPromise = (async () => {
    try {
      console.log("[Tailscale] Loading tsconnect module...");

      // Dynamic import of the tsconnect bundle
      const tsconnect = await import(JS_URL);
      createIPN = tsconnect.createIPN;
      runSSHSession = tsconnect.runSSHSession;

      if (!createIPN) {
        throw new Error("createIPN not found in tsconnect module");
      }

      wasmInitialized = true;
      console.log("[Tailscale] Module loaded successfully");
    } catch (error) {
      console.error("[Tailscale] Failed to load module:", error);
      initPromise = null;
      throw new Error(`Tailscale initialization failed: ${error.message}`);
    }
  })();

  return initPromise;
}

/**
 * TailscaleSession manages a connection to the tailnet via tsconnect WASM.
 *
 * Flow:
 * 1. Create session with control URL and pre-auth key
 * 2. Call connect() to join tailnet
 * 3. Wait for CONNECTED state
 * 4. Call openSSH() to create SSH connection to CLI
 */
export class TailscaleSession {
  constructor(controlUrl, preauthKey, hubIdentifier) {
    this.controlUrl = controlUrl;
    this.preauthKey = preauthKey;
    this.hubIdentifier = hubIdentifier || "default";
    this.state = ConnectionState.DISCONNECTED;
    this.ipn = null;
    this.netMap = null;
    this.stateStorage = null;

    // Callbacks
    this.onStateChange = null;
    this.onNetMapUpdate = null;
    this.onLog = null;
  }

  /**
   * Connect to the tailnet using the pre-auth key.
   *
   * @returns {Promise<void>} Resolves when connected (state is CONNECTED)
   */
  async connect() {
    if (!wasmInitialized) {
      throw new Error("Call initTailscale() first");
    }

    this.setState(ConnectionState.LOADING_WASM);
    this.log("Initializing Tailscale connection...");

    // Initialize state storage
    this.stateStorage = new IPNStateStorage(this.hubIdentifier);
    await this.stateStorage.init();
    await this.stateStorage.preload();

    // Generate a unique hostname for this browser session
    const hostname = this.generateHostname();

    try {
      // Create the IPN instance - this loads WASM and initializes
      this.ipn = await createIPN({
        wasmURL: WASM_URL,
        stateStorage: this.stateStorage,
        controlURL: this.controlUrl,
        authKey: this.preauthKey,
        hostname: hostname,
        panicHandler: (msg) => {
          console.error("[Tailscale] WASM panic:", msg);
          this.setState(ConnectionState.ERROR);
        },
      });

      this.setState(ConnectionState.CONNECTING);
      this.log(`Connecting to tailnet via ${this.controlUrl}...`);

      // Start the IPN with callbacks
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
          reject(new Error("Connection timeout (30s)"));
        }, 30000);

        this.ipn.run({
          notifyState: (state) => {
            const mappedState = STATE_MAP[state] || ConnectionState.CONNECTING;
            this.log(`Tailscale state: ${state} -> ${mappedState}`);
            this.setState(mappedState);

            if (mappedState === ConnectionState.CONNECTED) {
              clearTimeout(timeout);
              resolve();
            } else if (mappedState === ConnectionState.ERROR) {
              clearTimeout(timeout);
              reject(new Error(`Tailscale connection failed: ${state}`));
            }
          },

          notifyNetMap: (netMap) => {
            this.netMap = netMap;
            this.log(`NetMap updated: ${netMap.peers?.length || 0} peers`);
            this.onNetMapUpdate?.(netMap);
          },

          notifyBrowseToURL: (url) => {
            // This shouldn't happen with pre-auth key, but handle it
            this.log(`Auth required: ${url}`);
            this.setState(ConnectionState.NEEDS_LOGIN);
          },

          notifyPanicRecover: (err) => {
            console.error("[Tailscale] Recovered from panic:", err);
          },
        });
      });
    } catch (error) {
      this.setState(ConnectionState.ERROR);
      throw error;
    }
  }

  /**
   * Open an SSH connection to a peer on the tailnet.
   *
   * @param {string} hostname - Target hostname on the tailnet
   * @param {string} username - SSH username (default: "root")
   * @returns {Promise<SSHConnection>} SSH connection wrapper
   */
  async openSSH(hostname, username = "root") {
    if (!this.ipn || this.state !== ConnectionState.CONNECTED) {
      throw new Error("Not connected to tailnet");
    }

    this.log(`Opening SSH to ${hostname}@${username}...`);

    return new Promise((resolve, reject) => {
      const connection = new SSHConnection(hostname);

      const termConfig = {
        rows: 24,
        cols: 80,
        timeoutSeconds: 10,

        writeFn: (output) => {
          connection.handleOutput(output);
        },

        writeErrorFn: (msg) => {
          console.error("[SSH]", msg);
          connection.handleError(msg);
        },

        setReadFn: (readFn) => {
          // This gives us a function to call when we have input
          connection.setInputHandler(readFn);
        },

        onConnectionProgress: (msg) => {
          this.log(`SSH progress: ${msg}`);
        },

        onConnected: () => {
          this.log("SSH connected");
          connection.setConnected(true);
          resolve(connection);
        },

        onDone: () => {
          this.log("SSH session ended");
          connection.handleClose();
        },
      };

      try {
        // Start SSH session
        const sshSession = this.ipn.ssh(hostname, username, termConfig);
        connection.setSession(sshSession);
      } catch (error) {
        reject(new Error(`SSH connection failed: ${error.message}`));
      }
    });
  }

  /**
   * Disconnect from the tailnet.
   */
  async disconnect() {
    if (this.ipn) {
      try {
        this.ipn.logout();
      } catch {
        // Ignore errors during logout
      }
      this.ipn = null;
    }
    this.setState(ConnectionState.DISCONNECTED);
  }

  /**
   * Clear all stored state and disconnect.
   */
  async reset() {
    await this.disconnect();
    if (this.stateStorage) {
      await this.stateStorage.clear();
    }
  }

  isConnected() {
    return this.state === ConnectionState.CONNECTED;
  }

  /**
   * Get list of peers from current NetMap.
   *
   * @returns {Array} Peer list or empty array
   */
  getPeers() {
    if (!this.netMap?.peers) {
      return [];
    }
    return this.netMap.peers.map((p) => ({
      name: p.name,
      hostname: p.hostName,
      online: p.online,
      addresses: p.addresses,
      sshEnabled: p.sshHostKeys?.length > 0,
    }));
  }

  setState(state) {
    this.state = state;
    this.onStateChange?.(state);
  }

  log(message) {
    console.log(`[Tailscale] ${message}`);
    this.onLog?.(message);
  }

  generateHostname() {
    // Generate browser-{random} hostname
    const rand = Math.random().toString(36).substring(2, 8);
    return `browser-${rand}`;
  }
}

/**
 * SSH connection wrapper for terminal I/O.
 *
 * Wraps the tsconnect SSH session to provide a simpler interface
 * for terminal integration (xterm.js, etc).
 */
export class SSHConnection {
  constructor(hostname) {
    this.hostname = hostname;
    this.connected = false;
    this.session = null;
    this.inputHandler = null;

    // Callbacks
    this.onData = null;
    this.onError = null;
    this.onClose = null;
  }

  setSession(session) {
    this.session = session;
  }

  setInputHandler(handler) {
    this.inputHandler = handler;
  }

  setConnected(connected) {
    this.connected = connected;
  }

  /**
   * Write data to the SSH session (send to CLI).
   *
   * @param {string} data - Data to send
   */
  write(data) {
    if (!this.connected || !this.inputHandler) {
      console.warn("[SSH] Cannot write - not connected");
      return;
    }
    this.inputHandler(data);
  }

  /**
   * Resize the terminal.
   *
   * @param {number} cols - Number of columns
   * @param {number} rows - Number of rows
   */
  resize(cols, rows) {
    if (!this.connected || !this.session) {
      return;
    }
    try {
      this.session.resize(rows, cols);
    } catch (e) {
      console.warn("[SSH] Resize failed:", e);
    }
  }

  /**
   * Close the SSH connection.
   */
  close() {
    if (this.session) {
      try {
        this.session.close();
      } catch {
        // Ignore close errors
      }
      this.session = null;
    }
    this.connected = false;
  }

  // Internal handlers called by termConfig callbacks

  handleOutput(data) {
    this.onData?.(data);
  }

  handleError(msg) {
    this.onError?.(msg);
  }

  handleClose() {
    this.connected = false;
    this.onClose?.();
  }
}

/**
 * Parse the browser pre-auth key from URL fragment.
 *
 * The key is passed in the fragment (#key=xxx) so the server never sees it.
 *
 * @returns {string|null} Pre-auth key or null if not present
 */
export function parseKeyFromFragment() {
  const hash = window.location.hash;
  if (!hash || hash.length < 2) {
    return null;
  }

  const params = new URLSearchParams(hash.substring(1));
  return params.get("key");
}

/**
 * Get the Headscale control URL from meta tag or default.
 *
 * @returns {string} Control server URL
 */
export function getControlUrl() {
  const meta = document.querySelector('meta[name="headscale-url"]');
  if (meta) {
    return meta.getAttribute("content");
  }
  // Default to local development Headscale
  return `${window.location.protocol}//${window.location.hostname}:8080`;
}

/**
 * Get CLI hostname from URL fragment or meta tag.
 *
 * @returns {string|null} CLI hostname or null
 */
export function getCliHostname() {
  // Check URL fragment first
  const hash = window.location.hash;
  if (hash && hash.length > 1) {
    const params = new URLSearchParams(hash.substring(1));
    const hostname = params.get("cli");
    if (hostname) {
      return hostname;
    }
  }

  // Fall back to meta tag
  const meta = document.querySelector('meta[name="cli-hostname"]');
  if (meta) {
    return meta.getAttribute("content");
  }

  return null;
}
