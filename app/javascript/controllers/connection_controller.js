import { Controller } from "@hotwired/stimulus";
import {
  initTailscale,
  TailscaleSession,
  parseKeyFromFragment,
  getControlUrl,
  getCliHostname,
  ConnectionState,
} from "tailscale/index";

/**
 * Connection Controller - Tailscale E2E Encrypted Mesh
 *
 * This controller manages the secure connection between browser and CLI
 * via Tailscale/Headscale mesh networking.
 *
 * Architecture:
 * - Browser joins tailnet using pre-auth key from URL fragment
 * - CLI is already on the tailnet (same user's namespace)
 * - Browser opens SSH tunnel to CLI for terminal access
 * - All traffic is E2E encrypted via WireGuard
 *
 * Security:
 * - Pre-auth key is in URL fragment (#key=xxx), never sent to server
 * - Per-user tailnet isolation at Headscale infrastructure level
 * - WireGuard provides E2E encryption (server never sees plaintext)
 *
 * Usage in dependent controllers:
 * ```
 * connectionOutletConnected(outlet) {
 *   outlet.registerListener(this, {
 *     onConnected: (connection) => { ... },
 *     onDisconnected: () => { ... },
 *     onMessage: (message) => { ... },
 *     onError: (error) => { ... },
 *   });
 * }
 *
 * connectionOutletDisconnected(outlet) {
 *   outlet.unregisterListener(this);
 * }
 * ```
 */

export default class extends Controller {
  static targets = ["status"];

  static values = {
    hubIdentifier: String,
    cliHostname: String,
  };

  connect() {
    this.tailscaleSession = null;
    this.sshConnection = null;
    this.hubIdentifier = null;
    this.cliHostname = null;
    this.connected = false;

    // Don't overwrite listeners - outlet callbacks may have already registered
    // (Stimulus can call outlet callbacks before connect())
    if (!this.listeners) {
      this.listeners = new Map();
    }

    // Initialize and connect
    this.initializeConnection();
  }

  disconnect() {
    this.cleanup();
  }

  // ========== Listener Registration API ==========

  /**
   * Register a controller to receive connection callbacks.
   * If already connected, onConnected is called immediately.
   *
   * @param {Controller} controller - The Stimulus controller registering
   * @param {Object} callbacks - Callback functions
   * @param {Function} callbacks.onConnected - Called with connection when E2E established
   * @param {Function} callbacks.onDisconnected - Called when connection lost
   * @param {Function} callbacks.onMessage - Called with decrypted message from CLI
   * @param {Function} callbacks.onError - Called with error message
   */
  registerListener(controller, callbacks) {
    // Lazy init in case outlet callback fires before connect()
    if (!this.listeners) {
      this.listeners = new Map();
    }
    this.listeners.set(controller, callbacks);

    // If already connected, immediately notify
    if (this.connected && this.sshConnection) {
      callbacks.onConnected?.(this);
    }
  }

  /**
   * Unregister a controller from receiving callbacks.
   *
   * @param {Controller} controller - The controller to unregister
   */
  unregisterListener(controller) {
    this.listeners?.delete(controller);
  }

  // Notify all listeners of an event
  notifyListeners(event, data) {
    if (!this.listeners) return;
    for (const [, callbacks] of this.listeners) {
      switch (event) {
        case "connected":
          callbacks.onConnected?.(data);
          break;
        case "disconnected":
          callbacks.onDisconnected?.();
          break;
        case "message":
          callbacks.onMessage?.(data);
          break;
        case "error":
          callbacks.onError?.(data);
          break;
      }
    }
  }

  // ========== Connection Logic ==========

  async initializeConnection() {
    // Extract hub ID from URL path: /hubs/{hub_id}
    const pathMatch = window.location.pathname.match(/\/hubs\/([^\/]+)/);
    if (pathMatch) {
      this.hubIdentifier = pathMatch[1];
    } else if (this.hubIdentifierValue) {
      this.hubIdentifier = this.hubIdentifierValue;
    }

    if (!this.hubIdentifier) {
      this.emitError("Hub ID not found in URL");
      return;
    }

    // Get CLI hostname from fragment, meta tag, or API
    this.cliHostname = getCliHostname();
    if (!this.cliHostname && this.cliHostnameValue) {
      this.cliHostname = this.cliHostnameValue;
    }
    if (!this.cliHostname) {
      this.cliHostname = await this.fetchCliHostname();
    }

    if (!this.cliHostname) {
      this.emitError("CLI hostname not available - hub may be offline");
      return;
    }

    this.updateStatus("Initializing Tailscale...");

    try {
      // Initialize tsconnect WASM
      await initTailscale();

      // Get pre-auth key from URL fragment
      const preauthKey = parseKeyFromFragment();
      if (!preauthKey) {
        this.emitError(
          "No connection key found. Scan QR code from CLI to connect."
        );
        return;
      }

      // Get Headscale control URL
      const controlUrl = getControlUrl();

      // Create Tailscale session with hub identifier for state isolation
      this.tailscaleSession = new TailscaleSession(
        controlUrl,
        preauthKey,
        this.hubIdentifier
      );

      // Set up state change handler
      this.tailscaleSession.onStateChange = (state) => {
        this.handleStateChange(state);
      };

      // Set up logging
      this.tailscaleSession.onLog = (msg) => {
        console.log(`[Connection] ${msg}`);
      };

      // Connect to tailnet
      this.updateStatus("Joining tailnet...");
      await this.tailscaleSession.connect();

      // Open SSH tunnel to CLI
      this.updateStatus("Connecting to CLI...");
      await this.openSSHTunnel();

      this.connected = true;
      this.updateStatus(
        `Connected to ${this.hubIdentifier.substring(0, 8)}...`
      );

      // Notify all registered listeners
      this.notifyListeners("connected", this);
    } catch (error) {
      console.error("Failed to initialize connection:", error);
      this.emitError(`Connection error: ${error.message}`);
    }
  }

  async fetchCliHostname() {
    try {
      const response = await fetch(
        `/hubs/${this.hubIdentifier}/tailscale/status`,
        {
          headers: { Accept: "application/json" },
        }
      );

      if (!response.ok) {
        return null;
      }

      const data = await response.json();
      return data.hostname;
    } catch (error) {
      console.warn("Failed to fetch CLI hostname:", error);
      return null;
    }
  }

  async openSSHTunnel() {
    // Open SSH to CLI - username "root" is standard for Tailscale SSH
    this.sshConnection = await this.tailscaleSession.openSSH(
      this.cliHostname,
      "root"
    );

    // Set up data handler - receive terminal output from CLI
    this.sshConnection.onData = (data) => {
      this.handleSSHData(data);
    };

    // Set up error handler
    this.sshConnection.onError = (msg) => {
      console.error("[SSH Error]", msg);
    };

    // Set up close handler
    this.sshConnection.onClose = () => {
      this.handleSSHClose();
    };

    console.log("SSH tunnel established to CLI");
  }

  handleStateChange(state) {
    switch (state) {
      case ConnectionState.LOADING_WASM:
        this.updateStatus("Loading Tailscale...");
        break;
      case ConnectionState.CONNECTING:
        this.updateStatus("Connecting to tailnet...");
        break;
      case ConnectionState.NEEDS_LOGIN:
        this.updateStatus("Authentication required...");
        break;
      case ConnectionState.AUTHENTICATING:
        this.updateStatus("Authenticating...");
        break;
      case ConnectionState.STARTING:
        this.updateStatus("Starting connection...");
        break;
      case ConnectionState.CONNECTED:
        this.updateStatus("Connected to tailnet");
        break;
      case ConnectionState.ERROR:
        this.emitError("Tailnet connection error");
        break;
      case ConnectionState.DISCONNECTED:
        this.updateStatus("Disconnected");
        break;
    }
  }

  handleSSHData(data) {
    // Data is raw terminal output from CLI
    // Try to parse as JSON first (for structured messages)
    try {
      const text = typeof data === "string" ? data : new TextDecoder().decode(data);

      // Try to parse as JSON
      try {
        const message = JSON.parse(text);
        this.notifyListeners("message", message);
      } catch {
        // Not JSON - treat as raw terminal output
        this.notifyListeners("message", { type: "output", data: text });
      }
    } catch (error) {
      console.warn("Error handling SSH data:", error);
    }
  }

  handleSSHClose() {
    this.connected = false;
    this.updateStatus("Disconnected");
    this.notifyListeners("disconnected");
  }

  // ========== Public API for Outlets ==========

  /**
   * Send a JSON message to CLI via SSH.
   */
  send(type, data) {
    if (!this.sshConnection || !this.connected) {
      console.warn("Cannot send - not connected");
      return false;
    }

    const message = JSON.stringify({ type, ...data });
    this.sshConnection.write(message + "\n");
    return true;
  }

  /**
   * Send raw input to CLI (terminal keystrokes).
   */
  sendInput(inputData) {
    if (!this.sshConnection || !this.connected) {
      console.warn("Cannot send input - not connected");
      return false;
    }

    this.sshConnection.write(inputData);
    return true;
  }

  /**
   * Resize the terminal.
   */
  sendResize(cols, rows) {
    if (!this.sshConnection || !this.connected) {
      return false;
    }

    this.sshConnection.resize(cols, rows);
    return true;
  }

  requestAgents() {
    return this.send("list_agents", {});
  }

  selectAgent(agentId) {
    return this.send("select_agent", { agent_id: agentId });
  }

  isConnected() {
    return this.connected;
  }

  getHubIdentifier() {
    return this.hubIdentifier;
  }

  async resetSession() {
    if (this.tailscaleSession) {
      await this.tailscaleSession.reset();
    }
    this.cleanup();
    this.emitError("Session cleared. Scan QR code to reconnect.");
  }

  // ========== Cleanup ==========

  cleanup() {
    if (this.sshConnection) {
      this.sshConnection.close();
      this.sshConnection = null;
    }
    if (this.tailscaleSession) {
      this.tailscaleSession.disconnect();
      this.tailscaleSession = null;
    }
    this.connected = false;
    this.listeners?.clear();
  }

  disconnectAction() {
    this.cleanup();
    this.updateStatus("Disconnected");
    this.notifyListeners("disconnected");
  }

  // ========== Helpers ==========

  updateStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
    }
  }

  emitError(message) {
    this.updateStatus(message);
    this.notifyListeners("error", message);
  }
}
