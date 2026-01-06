import { Controller } from "@hotwired/stimulus";
import * as xterm from "@xterm/xterm";
import * as xtermFit from "@xterm/addon-fit";
import * as naclModule from "tweetnacl";
import * as naclUtilModule from "tweetnacl-util";
import consumer from "channels/consumer";

// Handle various ESM export styles
const Terminal = xterm.Terminal || xterm.default?.Terminal || xterm.default;
const FitAddon = xtermFit.FitAddon || xtermFit.default?.FitAddon || xtermFit.default;
const nacl = naclModule.default || naclModule;
const naclUtil = naclUtilModule.default || naclUtilModule;
const { encodeBase64, decodeBase64, encodeUTF8, decodeUTF8 } = naclUtil;

// IndexedDB storage for browser device keypair and paired CLI keys
const DB_NAME = "botster_device";
const DB_VERSION = 2; // Bumped for paired_keys store
const STORE_NAME = "keypair";
const PAIRED_KEYS_STORE = "paired_keys"; // Store CLI public keys by fingerprint

// Stimulus controller for E2E encrypted terminal access via Action Cable
// Rust guideline compliant 2025-01-05
export default class extends Controller {
  static targets = [
    "terminal",
    "status",
    "hubList",
    "connectButton",
    "disconnectButton",
    "selectedHub",
    "modeToggle",
    "sidePanel",
    "guiContainer",
    "tuiContainer",
    "hubListPanel",
    "agentListPanel",
    "agentActionsPanel",
    "agentList",
    "terminalPanel",
    "terminalTitle",
    "codeInputPanel",
    "codeInput",
    "codeError",
    // New Agent Modal
    "newAgentModal",
    "worktreeList",
    "issueOrBranch",
    "agentPrompt",
    // Close Agent Modal
    "closeAgentModal",
    "closeAgentName",
    // Terminal controls
    "ptyToggle",
    "scrollToLive",
    "selectedAgentLabel",
  ];

  static values = {
    csrfToken: String,
    browserDeviceId: Number,
    autoConnectHub: String,
    mode: { type: String, default: "tui" },
  };

  connect() {
    this.subscription = null;
    this.sharedSecret = null;
    this.selectedHubIdentifier = null;
    this.peerPublicKey = null;
    this.agents = [];
    this.selectedAgentId = null;
    this.worktrees = [];
    this.currentRepo = null;

    this.initTerminal();
    this.loadOrCreateKeypair();
    this.updateModeDisplay();

    // Listen for modal closed events to do cleanup
    this.boundHandleModalClosed = this.handleModalClosed.bind(this);
    this.element.addEventListener("modal:closed", this.boundHandleModalClosed);
  }

  // Handle modal closed events for cleanup
  handleModalClosed(event) {
    const modal = event.target;
    if (this.hasNewAgentModalTarget && modal === this.newAgentModalTarget) {
      this.cleanupNewAgentModal();
    }
  }

  disconnect() {
    if (this.boundHandleModalClosed) {
      this.element.removeEventListener("modal:closed", this.boundHandleModalClosed);
    }
    if (this.subscription) {
      this.subscription.unsubscribe();
    }
    if (this.terminal) {
      this.terminal.dispose();
    }
    if (this.resizeObserver) {
      this.resizeObserver.disconnect();
    }
    if (this.resizeTimeout) {
      clearTimeout(this.resizeTimeout);
    }
  }

  // Initialize xterm.js terminal
  initTerminal() {
    this.terminal = new Terminal({
      cursorBlink: true,
      fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
      fontSize: 14,
      theme: {
        background: "#1e1e1e",
        foreground: "#d4d4d4",
        cursor: "#ffffff",
      },
      allowProposedApi: true,
    });

    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.open(this.terminalTarget);

    // Handle terminal input - encrypt and send
    this.terminal.onData((data) => {
      if (this.subscription && this.sharedSecret) {
        this.sendEncrypted({ type: "input", data });
      }
    });

    // Handle window resize
    window.addEventListener("resize", () => this.handleResize());

    // Use ResizeObserver to detect container size changes (more reliable than window resize)
    this.resizeObserver = new ResizeObserver(() => {
      // Debounce rapid resize events
      if (this.resizeTimeout) clearTimeout(this.resizeTimeout);
      this.resizeTimeout = setTimeout(() => this.handleResize(), 50);
    });
    this.resizeObserver.observe(this.terminalTarget);

    // Handle mouse wheel scroll - send to CLI for scrollback
    this.terminalTarget.addEventListener("wheel", (event) => {
      if (this.subscription && this.sharedSecret && this.modeValue === "gui") {
        event.preventDefault();
        // Convert wheel delta to lines (roughly 3 lines per scroll tick)
        const lines = Math.ceil(Math.abs(event.deltaY) / 40);
        if (event.deltaY < 0) {
          // Scroll up (back in history)
          this.scrollUp(lines);
        } else {
          // Scroll down (forward toward live)
          this.scrollDown(lines);
        }
      }
    }, { passive: false });

    this.terminal.writeln("Botster Terminal - E2E Encrypted");
    this.terminal.writeln("Select a hub to connect...");

    // Wait for fonts to load before fitting terminal
    // This ensures character dimensions are calculated correctly
    this.waitForFontsAndFit();
  }

  // Wait for fonts to load before fitting terminal
  async waitForFontsAndFit() {
    try {
      // Wait for fonts to be ready (important for accurate character measurement)
      if (document.fonts && document.fonts.ready) {
        await document.fonts.ready;
      }
    } catch (e) {
      console.log("[Terminal] Font loading API not supported, continuing anyway");
    }

    // Multiple fit attempts to ensure correct sizing after layout stabilizes
    // First fit immediately after fonts
    this.handleResize();

    // Second fit after a short delay for any CSS animations/transitions
    setTimeout(() => this.handleResize(), 100);

    // Third fit after a longer delay in case of slow layout
    setTimeout(() => this.handleResize(), 500);
  }

  handleResize() {
    if (this.fitAddon && this.terminal) {
      const prevCols = this.terminal.cols;
      const prevRows = this.terminal.rows;

      // Debug: log container dimensions before fit
      const containerRect = this.terminalTarget.getBoundingClientRect();
      console.log(`[Terminal] Container size: ${Math.round(containerRect.width)}x${Math.round(containerRect.height)}px`);

      // Log xterm internal element dimensions for debugging
      const xtermElement = this.terminalTarget.querySelector('.xterm');
      const viewportElement = this.terminalTarget.querySelector('.xterm-viewport');
      const screenElement = this.terminalTarget.querySelector('.xterm-screen');
      if (xtermElement) {
        const xtermRect = xtermElement.getBoundingClientRect();
        console.log(`[Terminal] .xterm size: ${Math.round(xtermRect.width)}x${Math.round(xtermRect.height)}px`);
      }
      if (viewportElement) {
        const viewportRect = viewportElement.getBoundingClientRect();
        console.log(`[Terminal] .xterm-viewport size: ${Math.round(viewportRect.width)}x${Math.round(viewportRect.height)}px`);
      }
      if (screenElement) {
        const screenRect = screenElement.getBoundingClientRect();
        console.log(`[Terminal] .xterm-screen size: ${Math.round(screenRect.width)}x${Math.round(screenRect.height)}px`);
      }

      // Try to get proposed dimensions before fitting
      try {
        const proposed = this.fitAddon.proposeDimensions();
        if (proposed) {
          console.log(`[Terminal] Proposed dimensions: ${proposed.cols}x${proposed.rows}`);
        }
      } catch (e) {
        console.log(`[Terminal] Could not get proposed dimensions: ${e.message}`);
      }

      this.fitAddon.fit();

      const newCols = this.terminal.cols;
      const newRows = this.terminal.rows;

      // Always log for debugging
      console.log(`[Terminal] Fit result: ${newCols}x${newRows} cols/rows (was ${prevCols}x${prevRows})`);

      // Only send if dimensions actually changed
      if (newCols !== prevCols || newRows !== prevRows || !this.lastSentDims) {
        console.log(`[Terminal] Sending resize to CLI: ${newCols}x${newRows}`);
        this.lastSentDims = { cols: newCols, rows: newRows };

        if (this.subscription) {
          this.subscription.perform("resize", {
            cols: newCols,
            rows: newRows,
          });
        }
      }
    }
  }

  // Load keypair from IndexedDB or create new one
  async loadOrCreateKeypair() {
    try {
      const stored = await this.getStoredKeypair();
      if (stored) {
        this.keypair = {
          publicKey: decodeBase64(stored.publicKey),
          secretKey: decodeBase64(stored.secretKey),
        };
        this.updateStatus("Device loaded");
      } else {
        this.keypair = nacl.box.keyPair();
        await this.storeKeypair({
          publicKey: encodeBase64(this.keypair.publicKey),
          secretKey: encodeBase64(this.keypair.secretKey),
        });
        this.updateStatus("New device created");
        // Register with server
        await this.registerDevice();
      }

      // Auto-connect to hub if specified via URL parameter
      if (this.autoConnectHubValue) {
        this.autoConnectToHub(this.autoConnectHubValue);
      }
    } catch (error) {
      console.error("Failed to load/create keypair:", error);
      this.updateStatus("Crypto error - try refreshing");
    }
  }

  // IndexedDB operations
  openDB() {
    return new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, DB_VERSION);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
      request.onupgradeneeded = (event) => {
        const db = event.target.result;
        if (!db.objectStoreNames.contains(STORE_NAME)) {
          db.createObjectStore(STORE_NAME, { keyPath: "id" });
        }
        // Store for paired CLI keys (keyed by fingerprint)
        if (!db.objectStoreNames.contains(PAIRED_KEYS_STORE)) {
          db.createObjectStore(PAIRED_KEYS_STORE, { keyPath: "fingerprint" });
        }
      };
    });
  }

  async getStoredKeypair() {
    const db = await this.openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readonly");
      const store = tx.objectStore(STORE_NAME);
      const request = store.get("browser_device");
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
    });
  }

  async storeKeypair(keypair) {
    const db = await this.openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const request = store.put({ id: "browser_device", ...keypair });
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve();
    });
  }

  // Store a paired CLI's public key by fingerprint
  // This allows reconnecting without scanning QR code again
  async storePairedKey(fingerprint, publicKeyBase64, deviceName) {
    const db = await this.openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(PAIRED_KEYS_STORE, "readwrite");
      const store = tx.objectStore(PAIRED_KEYS_STORE);
      const request = store.put({
        fingerprint,
        publicKey: publicKeyBase64,
        deviceName: deviceName || "CLI Device",
        pairedAt: new Date().toISOString(),
      });
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve();
    });
  }

  // Get a paired CLI's public key by fingerprint
  async getPairedKey(fingerprint) {
    const db = await this.openDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(PAIRED_KEYS_STORE, "readonly");
      const store = tx.objectStore(PAIRED_KEYS_STORE);
      const request = store.get(fingerprint);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => resolve(request.result);
    });
  }

  // Register browser device with server
  async registerDevice() {
    try {
      const response = await fetch("/api/devices", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-CSRF-Token": this.csrfTokenValue,
        },
        body: JSON.stringify({
          public_key: encodeBase64(this.keypair.publicKey),
          device_type: "browser",
          name: this.browserName(),
        }),
      });

      if (response.ok) {
        const data = await response.json();
        console.log("Device registered:", data);
      }
    } catch (error) {
      console.error("Device registration failed:", error);
    }
  }

  browserName() {
    const ua = navigator.userAgent;
    if (ua.includes("Chrome")) return "Chrome Browser";
    if (ua.includes("Firefox")) return "Firefox Browser";
    if (ua.includes("Safari")) return "Safari Browser";
    return "Web Browser";
  }

  // Connect to a hub
  // NOTE: This requires server-assisted pairing to be enabled in user settings.
  // For MITM-proof connections, use the QR code flow (/agents/connect#key=...&hub=...)
  async connectToHub(event) {
    const hubIdentifier = event.currentTarget.dataset.hubIdentifier;

    if (!hubIdentifier) {
      this.updateStatus("No hub selected");
      return;
    }

    if (!this.keypair) {
      this.updateStatus("Waiting for device setup...");
      return;
    }

    this.selectedHubIdentifier = hubIdentifier;
    this.updateStatus(`Connecting to ${hubIdentifier}...`);

    // Fetch CLI device's public key for key exchange
    // This only works if user has enabled server-assisted pairing
    try {
      const response = await fetch(`/api/hubs/${hubIdentifier}/connection`, {
        headers: { "X-CSRF-Token": this.csrfTokenValue },
      });

      if (response.status === 403) {
        // Server-assisted pairing is disabled (default secure mode)
        const errorData = await response.json();

        // Check if we have a cached key for this CLI device
        if (errorData.device?.fingerprint) {
          const cached = await this.getPairedKey(errorData.device.fingerprint);
          if (cached) {
            console.log("Using cached CLI public key for fingerprint:", errorData.device.fingerprint);
            this.updateStatus("Using cached secure key...");

            // Use cached key directly
            const peerPublicKey = decodeBase64(cached.publicKey);
            this.peerPublicKey = peerPublicKey;
            this.sharedSecret = nacl.box.before(peerPublicKey, this.keypair.secretKey);
            this.selectedHubIdentifier = hubIdentifier;

            this.terminal.writeln("\r\n[Using cached secure key - previously paired via QR code]");
            this.subscribeToTerminal(hubIdentifier);
            return;
          }
        }

        // No cached key - show code input panel for manual key entry
        this.updateStatus("Secure mode - paste connection URL or scan QR code");
        this.showCodeInput(hubIdentifier, errorData.device?.fingerprint, errorData.device?.name);
        return;
      }

      if (!response.ok) {
        const error = await response.json();
        this.updateStatus(`Error: ${error.error}`);
        return;
      }

      const connectionInfo = await response.json();

      // Show warning if using server-assisted pairing
      if (connectionInfo.server_assisted_pairing) {
        this.terminal.writeln("\r\n[WARNING: Using server-assisted pairing (convenience mode)]");
        this.terminal.writeln("[For maximum security, scan QR code instead]");
        this.terminal.writeln("");
      }

      // Compute shared secret using Diffie-Hellman
      this.peerPublicKey = decodeBase64(connectionInfo.device.public_key);
      this.sharedSecret = nacl.box.before(
        this.peerPublicKey,
        this.keypair.secretKey,
      );

      // Subscribe to terminal channel
      this.subscribeToTerminal(hubIdentifier);
    } catch (error) {
      console.error("Connection failed:", error);
      this.updateStatus("Connection failed");
    }
  }

  // Auto-connect to a hub (called when hub parameter is in URL)
  // NOTE: This requires server-assisted pairing or cached keys. For secure mode, use /agents/connect#key=...&hub=...
  async autoConnectToHub(hubIdentifier) {
    if (!hubIdentifier) return;

    if (!this.keypair) {
      this.updateStatus("Waiting for device setup...");
      return;
    }

    this.selectedHubIdentifier = hubIdentifier;
    this.updateStatus(`Auto-connecting to ${hubIdentifier}...`);

    // Fetch CLI device's public key for key exchange
    // This only works if user has enabled server-assisted pairing
    try {
      const response = await fetch(`/api/hubs/${hubIdentifier}/connection`, {
        headers: { "X-CSRF-Token": this.csrfTokenValue },
      });

      if (response.status === 403) {
        // Server-assisted pairing is disabled (default secure mode)
        const errorData = await response.json();

        // Check if we have a cached key for this CLI device
        if (errorData.device?.fingerprint) {
          const cached = await this.getPairedKey(errorData.device.fingerprint);
          if (cached) {
            console.log("Using cached CLI public key for fingerprint:", errorData.device.fingerprint);
            this.updateStatus("Using cached secure key...");

            // Use cached key directly
            const peerPublicKey = decodeBase64(cached.publicKey);
            this.peerPublicKey = peerPublicKey;
            this.sharedSecret = nacl.box.before(peerPublicKey, this.keypair.secretKey);

            // Update selected hub display
            if (this.hasSelectedHubTarget) {
              this.selectedHubTarget.textContent = hubIdentifier.substring(0, 8) + "...";
            }

            this.terminal.writeln("\r\n[Using cached secure key - previously paired via QR code]");
            this.subscribeToTerminal(hubIdentifier);
            return;
          }
        }

        // No cached key - show code input panel for manual key entry
        this.updateStatus("Secure mode - paste connection URL or scan QR code");
        this.showCodeInput(hubIdentifier, errorData.device?.fingerprint, errorData.device?.name);
        return;
      }

      if (!response.ok) {
        const error = await response.json();
        this.updateStatus(`Error: ${error.error}`);
        return;
      }

      const connectionInfo = await response.json();

      // Show warning if using server-assisted pairing
      if (connectionInfo.server_assisted_pairing) {
        this.terminal.writeln("\r\n[Using server-assisted pairing (convenience mode)]");
        this.terminal.writeln("[For maximum security, scan QR code instead]");
        this.terminal.writeln("");
      }

      // Compute shared secret using Diffie-Hellman
      this.peerPublicKey = decodeBase64(connectionInfo.device.public_key);
      this.sharedSecret = nacl.box.before(
        this.peerPublicKey,
        this.keypair.secretKey,
      );

      // Update selected hub display
      if (this.hasSelectedHubTarget) {
        this.selectedHubTarget.textContent =
          hubIdentifier.substring(0, 8) + "...";
      }

      // Subscribe to terminal channel
      this.subscribeToTerminal(hubIdentifier);
    } catch (error) {
      console.error("Auto-connection failed:", error);
      this.updateStatus("Connection failed - hub may be offline");
    }
  }

  subscribeToTerminal(hubIdentifier) {
    if (this.subscription) {
      this.subscription.unsubscribe();
    }

    this.subscription = consumer.subscriptions.create(
      {
        channel: "TerminalChannel",
        hub_identifier: hubIdentifier,
        device_type: "browser",
      },
      {
        connected: () => {
          this.updateStatus(`Connected to ${hubIdentifier}`);
          this.terminal.clear();
          this.terminal.writeln(`Connected to hub: ${hubIdentifier}`);
          this.terminal.writeln("E2E encryption active");
          this.terminal.writeln("");

          // Announce presence with public key for E2E key exchange
          this.subscription.perform("presence", {
            event: "join",
            device_name: this.browserName(),
            public_key: encodeBase64(this.keypair.publicKey),
          });

          // Fit terminal to container before sending dimensions
          // This ensures we send accurate dimensions on connect
          if (this.fitAddon) {
            this.fitAddon.fit();
          }

          // Send terminal size
          const cols = this.terminal.cols;
          const rows = this.terminal.rows;
          console.log(`[Terminal] Connected - sending initial size: ${cols}x${rows}`);
          this.subscription.perform("resize", {
            cols: cols,
            rows: rows,
          });
          this.lastSentDims = { cols, rows };

          // Update mode display now that we're connected
          this.updateModeDisplay();

          // Request agent list if in GUI mode
          if (this.modeValue === "gui") {
            this.requestAgentList();
          }
        },

        disconnected: () => {
          this.updateStatus("Disconnected");
          this.terminal.writeln("\r\n[Disconnected from hub]");
        },

        rejected: () => {
          this.updateStatus("Connection rejected");
        },

        received: (data) => {
          this.handleMessage(data);
        },
      },
    );
  }

  handleMessage(data) {
    console.log("[Terminal] Received message:", data.type, data.from);
    switch (data.type) {
      case "terminal":
        // Ignore our own messages
        if (data.from === "browser") return;
        console.log("[Terminal] Decrypting terminal data from CLI");
        this.receiveEncrypted(data);
        break;

      case "presence":
        if (data.event === "join") {
          this.terminal.writeln(
            `\r\n[${data.device_name || data.device_type} connected]`,
          );
        } else if (data.event === "leave") {
          this.terminal.writeln(`\r\n[${data.device_type} disconnected]`);
        }
        break;

      case "resize":
        // CLI sent resize - we could adjust but browser controls its own size
        break;
    }
  }

  // Encrypt and send data to CLI
  sendEncrypted(message) {
    const nonce = nacl.randomBytes(nacl.box.nonceLength);
    const messageBytes = decodeUTF8(JSON.stringify(message));
    const encrypted = nacl.box.after(messageBytes, nonce, this.sharedSecret);

    this.subscription.perform("relay", {
      blob: encodeBase64(encrypted),
      nonce: encodeBase64(nonce),
    });
  }

  // Decrypt received data from CLI
  receiveEncrypted(data) {
    try {
      const blob = decodeBase64(data.blob);
      const nonce = decodeBase64(data.nonce);
      console.log("[Terminal] Decrypting blob of size:", blob.length);
      const decrypted = nacl.box.open.after(blob, nonce, this.sharedSecret);

      if (!decrypted) {
        console.error("Decryption failed - invalid shared secret?");
        return;
      }

      const message = JSON.parse(encodeUTF8(decrypted));
      console.log("[Terminal] Decrypted message type:", message.type, "data length:", message.data?.length);

      switch (message.type) {
        case "output":
          console.log("[Terminal] Writing output to xterm, length:", message.data.length);
          this.terminal.write(message.data);
          break;

        case "screen":
          // Full TUI screen update (TUI mode)
          if (this.modeValue === "tui" && message.data) {
            const binaryString = atob(message.data);
            const bytes = Uint8Array.from(binaryString, (c) => c.charCodeAt(0));
            const data = new TextDecoder().decode(bytes);
            this.terminal.write(data);
          }
          break;

        case "agent_output":
          // Individual agent terminal output (GUI mode)
          if (this.modeValue === "gui" && message.id === this.selectedAgentId && message.data) {
            const binaryString = atob(message.data);
            const bytes = Uint8Array.from(binaryString, (c) => c.charCodeAt(0));
            const data = new TextDecoder().decode(bytes);
            this.terminal.write(data);
          }
          break;

        case "agents":
        case "agent_list":
          console.log("[Terminal] Received agent list:", message.agents);
          this.renderAgentList(message.agents || []);
          break;

        case "agent_selected":
          console.log("[Terminal] Agent selected:", message.id);
          this.selectedAgentId = message.id;
          this.renderAgentList(this.agents);
          this.updateSelectedAgentLabel();
          break;

        case "agent_created":
          console.log("[Terminal] Agent created:", message.id || message.agent_id);
          this.selectedAgentId = message.id || message.agent_id;
          if (this.terminal) {
            this.terminal.clear();
          }
          this.requestAgentList();
          break;

        case "agent_deleted":
        case "agent_closed":
          console.log("[Terminal] Agent closed:", message.id || message.agent_id);
          const closedId = message.id || message.agent_id;
          if (this.selectedAgentId === closedId) {
            this.selectedAgentId = null;
            if (this.terminal) {
              this.terminal.clear();
            }
          }
          this.requestAgentList();
          break;

        case "worktrees":
          console.log("[Terminal] Received worktrees:", message.worktrees);
          this.worktrees = message.worktrees || [];
          this.currentRepo = message.repo;
          this.renderWorktreeList();
          break;

        case "error":
          console.error("[Terminal] Error from CLI:", message.error || message.message);
          this.terminal.writeln(`\r\n[Error: ${message.error || message.message}]`);
          break;

        default:
          console.log("[Terminal] Unknown message type:", message.type);
      }
    } catch (error) {
      console.error("Failed to decrypt message:", error);
    }
  }

  updateStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text;
    }
  }

  // Disconnect from current hub
  disconnectHub() {
    if (this.subscription) {
      this.subscription.unsubscribe();
      this.subscription = null;
    }
    this.sharedSecret = null;
    this.selectedHubIdentifier = null;
    this.selectedAgentId = null;
    this.agents = [];
    this.hideCodeInput();
    this.updateStatus("Disconnected");
    this.terminal.writeln("\r\n[Disconnected]");
    this.updateModeDisplay();
  }

  // Show code input panel for secure mode connection
  showCodeInput(hubIdentifier, deviceFingerprint, deviceName) {
    this.pendingHubIdentifier = hubIdentifier;
    this.pendingDeviceFingerprint = deviceFingerprint;
    this.pendingDeviceName = deviceName;
    if (this.hasCodeInputPanelTarget) {
      this.codeInputPanelTarget.classList.remove("hidden");
      if (this.hasCodeInputTarget) {
        this.codeInputTarget.value = "";
        this.codeInputTarget.focus();
      }
      if (this.hasCodeErrorTarget) {
        this.codeErrorTarget.classList.add("hidden");
        this.codeErrorTarget.textContent = "";
      }
    }
    this.terminal.writeln("\r\n[Secure mode active - paste connection URL above]");
    this.terminal.writeln("[Press 'm' in your CLI to show the QR code with URL]");
  }

  // Hide code input panel
  hideCodeInput() {
    this.pendingHubIdentifier = null;
    this.pendingDeviceFingerprint = null;
    this.pendingDeviceName = null;
    if (this.hasCodeInputPanelTarget) {
      this.codeInputPanelTarget.classList.add("hidden");
    }
    if (this.hasCodeInputTarget) {
      this.codeInputTarget.value = "";
    }
    if (this.hasCodeErrorTarget) {
      this.codeErrorTarget.classList.add("hidden");
    }
  }

  // Compute fingerprint from public key (first 8 bytes of SHA256 as hex:hex:...)
  async computeFingerprint(publicKey) {
    const hashBuffer = await crypto.subtle.digest("SHA-256", publicKey);
    const hashArray = Array.from(new Uint8Array(hashBuffer).slice(0, 8));
    return hashArray.map((b) => b.toString(16).padStart(2, "0")).join(":");
  }

  // Connect using pasted code/URL
  async connectWithCode() {
    if (!this.hasCodeInputTarget) return;

    const input = this.codeInputTarget.value.trim();
    if (!input) {
      this.showCodeError("Please paste the connection URL from your CLI");
      return;
    }

    // Parse the URL to extract key and hub from fragment
    // Format: https://server/agents/connect#key=BASE64URL&hub=IDENTIFIER
    try {
      let keyBase64Url, hubIdentifier;

      // Check if it's a full URL or just the fragment part
      if (input.includes("#")) {
        const url = new URL(input);
        const fragment = url.hash.substring(1); // Remove the #
        const params = new URLSearchParams(fragment);
        keyBase64Url = params.get("key");
        hubIdentifier = params.get("hub");
      } else if (input.includes("key=")) {
        // Just the fragment content without #
        const params = new URLSearchParams(input);
        keyBase64Url = params.get("key");
        hubIdentifier = params.get("hub");
      } else {
        // Might be just the key itself
        keyBase64Url = input;
        hubIdentifier = this.pendingHubIdentifier;
      }

      if (!keyBase64Url) {
        this.showCodeError("Could not find key in URL. Make sure you copied the full URL.");
        return;
      }

      if (!hubIdentifier) {
        this.showCodeError("Could not find hub identifier. Please copy the full URL.");
        return;
      }

      // Convert from base64url to standard base64 (with padding)
      let keyBase64 = keyBase64Url.replace(/-/g, "+").replace(/_/g, "/");
      // Add padding if necessary (base64 requires length to be multiple of 4)
      const padding = keyBase64.length % 4;
      if (padding === 2) {
        keyBase64 += "==";
      } else if (padding === 3) {
        keyBase64 += "=";
      }

      // Decode the public key
      const peerPublicKey = decodeBase64(keyBase64);
      if (!peerPublicKey || peerPublicKey.length !== 32) {
        this.showCodeError("Invalid key format. Please copy the URL again.");
        return;
      }

      // Compute shared secret using Diffie-Hellman
      this.peerPublicKey = peerPublicKey;
      this.sharedSecret = nacl.box.before(this.peerPublicKey, this.keypair.secretKey);
      this.selectedHubIdentifier = hubIdentifier;

      // Save the CLI public key for future connections (cached secure pairing)
      // Use the fingerprint from the API response, or compute it from the key
      const fingerprint = this.pendingDeviceFingerprint || await this.computeFingerprint(peerPublicKey);
      await this.storePairedKey(fingerprint, keyBase64, this.pendingDeviceName);
      console.log("Saved paired CLI key with fingerprint:", fingerprint);

      // Hide code input and show success
      this.hideCodeInput();
      this.terminal.writeln("\r\n[Key exchange complete - MITM-proof connection]");
      this.terminal.writeln("[Key saved for future connections]");
      this.updateStatus(`Connecting to ${hubIdentifier}...`);

      // Update selected hub display
      if (this.hasSelectedHubTarget) {
        this.selectedHubTarget.textContent = hubIdentifier.substring(0, 8) + "...";
      }

      // Subscribe to terminal channel
      this.subscribeToTerminal(hubIdentifier);
    } catch (error) {
      console.error("Failed to parse connection code:", error);
      this.showCodeError("Invalid URL format. Please copy the full URL from your CLI.");
    }
  }

  // Show error message in code input panel
  showCodeError(message) {
    if (this.hasCodeErrorTarget) {
      this.codeErrorTarget.textContent = message;
      this.codeErrorTarget.classList.remove("hidden");
    }
  }

  // Special key handlers for mobile touch controls
  sendCtrlC() {
    this.sendKey("\x03");
  }

  sendEnter() {
    this.sendKey("\r");
  }

  sendEscape() {
    this.sendKey("\x1b");
  }

  sendTab() {
    this.sendKey("\t");
  }

  sendKey(key) {
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({ type: "input", data: key });
    }
  }

  // Arrow key handlers for mobile touch controls
  sendArrowUp() {
    this.sendKey("\x1b[A");
  }

  sendArrowDown() {
    this.sendKey("\x1b[B");
  }

  sendArrowRight() {
    this.sendKey("\x1b[C");
  }

  sendArrowLeft() {
    this.sendKey("\x1b[D");
  }

  // Mode toggle between TUI and GUI
  toggleMode() {
    this.modeValue = this.modeValue === "tui" ? "gui" : "tui";
    this.updateModeDisplay();

    // Request agent list when switching to GUI mode
    if (this.modeValue === "gui" && this.subscription) {
      this.requestAgentList();
    }
  }

  updateModeDisplay() {
    const isTui = this.modeValue === "tui";
    const isConnected = !!this.subscription;

    // Update toggle button text
    if (this.hasModeToggleTarget) {
      this.modeToggleTarget.textContent = isTui ? "Switch to GUI" : "Switch to TUI";
    }

    // Update terminal title
    if (this.hasTerminalTitleTarget) {
      this.terminalTitleTarget.textContent = isTui ? "Hub Terminal (TUI)" : "Agent Terminal";
    }

    // Show/hide disconnect button
    if (this.hasDisconnectButtonTarget) {
      this.disconnectButtonTarget.classList.toggle("hidden", !isConnected);
    }

    // Show/hide panels based on mode and connection status
    if (this.hasHubListPanelTarget) {
      // Hub list shown when not connected
      this.hubListPanelTarget.classList.toggle("hidden", isConnected);
    }

    if (this.hasAgentListPanelTarget) {
      // Agent list shown only in GUI mode when connected
      this.agentListPanelTarget.classList.toggle("hidden", isTui || !isConnected);
    }

    if (this.hasAgentActionsPanelTarget) {
      // Agent actions shown only in GUI mode when connected (desktop only)
      this.agentActionsPanelTarget.classList.toggle("hidden", isTui || !isConnected);
    }

    // Handle GUI container (sidebar)
    if (this.hasGuiContainerTarget) {
      // GUI container hidden in TUI mode when connected
      this.guiContainerTarget.classList.toggle("hidden", isTui && isConnected);
    }

    // Handle terminal container width
    if (this.hasTuiContainerTarget) {
      if (isTui && isConnected) {
        // TUI mode when connected: terminal takes full width
        this.tuiContainerTarget.classList.remove("lg:col-span-3");
        this.tuiContainerTarget.classList.add("lg:col-span-4");
      } else {
        // GUI mode or not connected: show side panel
        this.tuiContainerTarget.classList.remove("lg:col-span-4");
        this.tuiContainerTarget.classList.add("lg:col-span-3");
      }
    }

    // Legacy: handle terminalPanel if it exists
    if (this.hasTerminalPanelTarget) {
      if (isTui && isConnected) {
        this.terminalPanelTarget.classList.remove("lg:col-span-3");
        this.terminalPanelTarget.classList.add("lg:col-span-4");
      } else {
        this.terminalPanelTarget.classList.remove("lg:col-span-4");
        this.terminalPanelTarget.classList.add("lg:col-span-3");
      }
    }

    if (this.hasSidePanelTarget) {
      // Side panel hidden in TUI mode when connected
      this.sidePanelTarget.classList.toggle("hidden", isTui && isConnected);
    }

    // Send mode to CLI so it knows what data to send
    this.sendMode();

    // Refit terminal after layout change
    setTimeout(() => this.handleResize(), 100);
  }

  // Send current mode to CLI
  sendMode() {
    if (this.subscription && this.sharedSecret) {
      console.log(`Sending mode: ${this.modeValue}`);
      this.sendEncrypted({ type: "set_mode", mode: this.modeValue });
    }
  }

  // Agent list management
  requestAgentList() {
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({ type: "list_agents" });
    }
  }

  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId;
    console.log("Selecting agent:", agentId);

    if (this.terminal) {
      this.terminal.clear();
    }

    this.selectedAgentId = agentId;
    this.renderAgentList(this.agents);
    this.updateSelectedAgentLabel();

    // Switch to this agent's view
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({ type: "select_agent", id: agentId });
    }

    // Send resize for the newly selected agent
    setTimeout(() => this.handleResize(), 100);
  }

  renderAgentList(agents) {
    this.agents = agents;
    if (!this.hasAgentListTarget) return;

    if (agents.length === 0) {
      this.agentListTarget.innerHTML = `
        <div class="text-gray-500 text-center py-4 lg:py-8">
          <p class="text-sm">No agents running</p>
          <p class="text-xs mt-2">Use "New Agent" to create one</p>
        </div>
      `;
      return;
    }

    const html = agents
      .map((agent) => {
        const isSelected = agent.id === this.selectedAgentId;
        const issueLabel = agent.issue_number
          ? `#${agent.issue_number}`
          : agent.branch_name || agent.name || `Agent ${agent.id.substring(0, 8)}`;
        const statusColor =
          agent.status === "Running" ? "text-green-600" : "text-gray-500";

        // Build preview/server status indicator
        let serverBadge = "";
        if (agent.tunnel_port) {
          if (agent.server_running) {
            serverBadge = `<a href="/preview/${agent.hub_identifier}/${agent.id}" target="_blank"
               class="inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium rounded-full bg-green-100 text-green-800 hover:bg-green-200"
               onclick="event.stopPropagation()">
               <span class="w-1.5 h-1.5 rounded-full bg-green-500"></span>
               :${agent.tunnel_port}
             </a>`;
          } else {
            serverBadge = `<span class="inline-flex items-center gap-1 px-2 py-0.5 text-xs font-medium rounded-full bg-gray-100 text-gray-600">
               <span class="w-1.5 h-1.5 rounded-full bg-gray-400"></span>
               :${agent.tunnel_port}
             </span>`;
          }
        }

        // PTY view indicator
        let ptyViewBadge = "";
        if (agent.has_server_pty) {
          const viewLabel = agent.active_pty_view === "server" ? "SRV" : "CLI";
          const viewColor = agent.active_pty_view === "server" ? "bg-purple-100 text-purple-800" : "bg-blue-100 text-blue-800";
          ptyViewBadge = `<span class="inline-flex items-center px-1.5 py-0.5 text-xs font-medium rounded ${viewColor}">${viewLabel}</span>`;
        }

        // Scroll indicator
        let scrollBadge = "";
        if (agent.scroll_offset > 0) {
          scrollBadge = `<span class="inline-flex items-center px-1.5 py-0.5 text-xs font-medium rounded bg-yellow-100 text-yellow-800">↑${agent.scroll_offset}</span>`;
        }

        return `
        <button
          type="button"
          data-action="terminal#selectAgent"
          data-agent-id="${agent.id}"
          class="w-full text-left px-4 py-3 border-b border-gray-200 hover:bg-gray-50 transition-colors ${isSelected ? "bg-blue-50 border-l-4 border-l-blue-500" : ""}"
        >
          <div class="flex items-center justify-between">
            <div>
              <span class="font-medium text-gray-900">${agent.repo || "Agent"}</span>
              <span class="text-gray-600 ml-2">${issueLabel}</span>
            </div>
            <div class="flex items-center gap-2">
              ${scrollBadge}
              ${ptyViewBadge}
              ${serverBadge}
              <span class="${statusColor} text-sm">${agent.status || "Running"}</span>
            </div>
          </div>
        </button>
      `;
      })
      .join("");

    this.agentListTarget.innerHTML = html;
  }

  // Update selected agent label
  updateSelectedAgentLabel() {
    if (!this.hasSelectedAgentLabelTarget) return;

    if (!this.selectedAgentId) {
      this.selectedAgentLabelTarget.textContent = "No agent selected";
      return;
    }

    const agent = this.agents.find((a) => a.id === this.selectedAgentId);
    if (agent) {
      const issueLabel = agent.issue_number
        ? `#${agent.issue_number}`
        : agent.branch_name || agent.name || agent.id.substring(0, 8);

      let label = `${agent.repo || "Agent"} ${issueLabel}`;
      if (agent.has_server_pty) {
        const viewLabel = agent.active_pty_view === "server" ? "[Server]" : "[CLI]";
        label += ` ${viewLabel}`;
      }
      if (agent.scroll_offset > 0) {
        label += ` [↑${agent.scroll_offset}]`;
      }

      this.selectedAgentLabelTarget.textContent = label;
    }
  }

  // Get the currently selected agent
  getSelectedAgent() {
    return this.agents.find((a) => a.id === this.selectedAgentId);
  }

  // ========== New Agent Modal ==========

  showNewAgentModal() {
    if (!this.hasNewAgentModalTarget) return;

    // Show modal via modal controller
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:show"));

    // Request available worktrees from CLI
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({ type: "list_worktrees" });
    }

    // Focus the issue/branch input
    if (this.hasIssueOrBranchTarget) {
      setTimeout(() => this.issueOrBranchTarget.focus(), 100);
    }
  }

  // Cleanup when new agent modal closes
  cleanupNewAgentModal() {
    if (this.hasIssueOrBranchTarget) this.issueOrBranchTarget.value = "";
    if (this.hasAgentPromptTarget) this.agentPromptTarget.value = "";
    this.worktrees = [];
    if (this.hasWorktreeListTarget) {
      this.worktreeListTarget.innerHTML = "";
    }
  }

  // Render available worktrees in the modal
  renderWorktreeList() {
    if (!this.hasWorktreeListTarget) return;

    if (this.worktrees.length === 0) {
      this.worktreeListTarget.innerHTML = `
        <p class="text-sm text-gray-500 py-3 px-3 text-center">No existing worktrees available</p>
      `;
      return;
    }

    const html = this.worktrees
      .map((wt, index) => {
        const issueLabel = wt.issue_number ? `#${wt.issue_number}` : wt.branch;
        const isLast = index === this.worktrees.length - 1;
        const borderClass = isLast ? "" : "border-b border-gray-200";
        return `
        <button
          type="button"
          data-action="terminal#selectWorktree"
          data-worktree-path="${wt.path}"
          data-worktree-branch="${wt.branch}"
          class="w-full text-left px-3 py-2 text-sm hover:bg-blue-50 ${borderClass} flex justify-between items-center group"
        >
          <div>
            <span class="font-medium text-gray-900">${issueLabel}</span>
            <span class="text-gray-500 ml-2">${wt.branch}</span>
          </div>
          <span class="text-blue-600 text-xs opacity-0 group-hover:opacity-100">Click to reopen →</span>
        </button>
      `;
      })
      .join("");

    this.worktreeListTarget.innerHTML = html;
  }

  // Select an existing worktree to reopen
  selectWorktree(event) {
    const path = event.currentTarget.dataset.worktreePath;
    const branch = event.currentTarget.dataset.worktreeBranch;
    const prompt = this.hasAgentPromptTarget
      ? this.agentPromptTarget.value.trim()
      : null;

    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({
        type: "reopen_worktree",
        path: path,
        branch: branch,
        prompt: prompt || null,
      });
    }

    // Close modal
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  submitNewAgent() {
    if (!this.hasIssueOrBranchTarget) return;

    // Use native HTML5 validation
    if (!this.issueOrBranchTarget.reportValidity()) {
      return;
    }

    const issueOrBranch = this.issueOrBranchTarget.value.trim();
    const prompt = this.hasAgentPromptTarget
      ? this.agentPromptTarget.value.trim()
      : null;

    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({
        type: "create_agent",
        issue_or_branch: issueOrBranch,
        prompt: prompt || null,
      });
    }

    // Close modal
    this.newAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  // ========== Close Agent Modal ==========

  showCloseAgentModal() {
    if (!this.selectedAgentId) {
      alert("Please select an agent first");
      return;
    }
    if (!this.hasCloseAgentModalTarget) return;

    // Update modal with agent info
    const agent = this.getSelectedAgent();
    if (agent && this.hasCloseAgentNameTarget) {
      const issueLabel = agent.issue_number
        ? `#${agent.issue_number}`
        : agent.branch_name || agent.name || agent.id.substring(0, 8);
      this.closeAgentNameTarget.textContent = `${agent.repo || "Agent"} ${issueLabel}`;
    }

    // Show modal
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:show"));
  }

  closeAgentKeepWorktree() {
    if (!this.selectedAgentId) return;
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({
        type: "delete_agent",
        id: this.selectedAgentId,
        delete_worktree: false,
      });
    }
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  closeAgentDeleteWorktree() {
    if (!this.selectedAgentId) return;
    if (this.subscription && this.sharedSecret) {
      this.sendEncrypted({
        type: "delete_agent",
        id: this.selectedAgentId,
        delete_worktree: true,
      });
    }
    this.closeAgentModalTarget.dispatchEvent(new CustomEvent("modal:hide"));
  }

  // ========== PTY View Toggle ==========

  togglePtyView() {
    if (!this.subscription || !this.sharedSecret) {
      console.warn("Cannot toggle PTY view - not connected");
      return;
    }
    console.log("Sending toggle PTY view");
    this.sendEncrypted({ type: "toggle_pty_view" });

    // Clear terminal when switching views
    if (this.terminal) {
      this.terminal.clear();
    }
  }

  // ========== Scroll Controls ==========

  scrollToBottom() {
    if (!this.subscription || !this.sharedSecret) {
      console.warn("Cannot scroll - not connected");
      return;
    }
    console.log("Sending scroll to bottom");
    this.sendEncrypted({ type: "scroll_to_bottom" });
  }

  scrollUp(lines = 3) {
    if (!this.subscription || !this.sharedSecret) return;
    this.sendEncrypted({ type: "scroll", direction: "up", lines: lines });
  }

  scrollDown(lines = 3) {
    if (!this.subscription || !this.sharedSecret) return;
    this.sendEncrypted({ type: "scroll", direction: "down", lines: lines });
  }

  // ========== Tunnel Cache Clear ==========

  async clearTunnelCache() {
    const agent = this.getSelectedAgent();
    if (!agent) {
      alert("No agent selected");
      return;
    }

    if (!agent.hub_identifier) {
      alert("Agent has no hub identifier");
      return;
    }

    const scope = `/preview/${agent.hub_identifier}/${agent.id}/`;
    const cookiePath = `/preview/${agent.hub_identifier}/${agent.id}`;

    try {
      // Unregister service worker for this scope
      if ("serviceWorker" in navigator) {
        const registrations = await navigator.serviceWorker.getRegistrations();
        for (const registration of registrations) {
          if (registration.scope.includes(scope) || registration.scope.includes(cookiePath)) {
            await registration.unregister();
            console.log(`Unregistered SW for scope: ${registration.scope}`);
          }
        }
      }

      // Clear the tunnel_sw cookie
      document.cookie = `tunnel_sw=; path=${cookiePath}; expires=Thu, 01 Jan 1970 00:00:00 GMT; SameSite=Strict`;
      document.cookie = `tunnel_sw=; path=${scope}; expires=Thu, 01 Jan 1970 00:00:00 GMT; SameSite=Strict`;

      alert(`Cleared tunnel cache for ${agent.id}\n\nScope: ${scope}\n\nRefresh the preview page to re-initialize.`);
    } catch (error) {
      console.error("Failed to clear tunnel cache:", error);
      alert(`Error clearing tunnel cache: ${error.message}`);
    }
  }
}
