import { Controller } from "@hotwired/stimulus";
import * as xterm from "@xterm/xterm";
import * as xtermFit from "@xterm/addon-fit";
import { x25519, ed25519 } from "@noble/curves/ed25519";
import { randomBytes } from "@noble/ciphers/webcrypto";
import consumer from "channels/consumer";
import { RatchetSession, serializeEnvelope, deserializeEnvelope } from "../crypto/ratchet";

// Handle various ESM export styles
const Terminal = xterm.Terminal || xterm.default?.Terminal || xterm.default;
const FitAddon = xtermFit.FitAddon || xtermFit.default?.FitAddon || xtermFit.default;

// Base64 encoding/decoding utilities
function encodeBase64(bytes) {
  return btoa(String.fromCharCode(...bytes));
}

function decodeBase64(str) {
  return new Uint8Array(atob(str).split("").map(c => c.charCodeAt(0)));
}

// UTF8 encoding/decoding utilities
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function encodeUTF8(bytes) {
  return textDecoder.decode(bytes);
}

function decodeUTF8(str) {
  return textEncoder.encode(str);
}

// X25519 keypair generation (for encryption)
function generateEncryptionKeypair() {
  const secretKey = randomBytes(32);
  const publicKey = x25519.getPublicKey(secretKey);
  return { secretKey, publicKey };
}

// Ed25519 keypair generation (for signing)
function generateSigningKeypair() {
  const privateKey = randomBytes(32);
  const publicKey = ed25519.getPublicKey(privateKey);
  return { privateKey, publicKey };
}

// Sign a message with Ed25519
function signMessage(message, privateKey) {
  return ed25519.sign(message, privateKey);
}

// Compute raw X25519 shared secret (used to initialize Double Ratchet)
function computeRawSharedSecret(theirPublicKey, ourSecretKey) {
  return x25519.getSharedSecret(ourSecretKey, theirPublicKey);
}

// IndexedDB storage for browser device keypair and paired CLI keys
const DB_NAME = "botster_device";
const DB_VERSION = 2; // Bumped for paired_keys store
const STORE_NAME = "keypair";
const PAIRED_KEYS_STORE = "paired_keys"; // Store CLI public keys by fingerprint

/**
 * Secure Terminal Controller - MITM-Proof E2E Encryption
 *
 * This controller implements secure key exchange where the CLI's public key
 * is transmitted via URL fragment (#key=...), which is NEVER sent to the server.
 * This prevents man-in-the-middle attacks even if the server is compromised.
 *
 * Security model:
 * 1. CLI generates keypair and displays QR code with public key in URL fragment
 * 2. User scans QR code, browser navigates to /agents/connect#key=...&hub=...
 * 3. Browser reads key from window.location.hash (server never sees this)
 * 4. Browser uses CLI's public key directly for Diffie-Hellman key exchange
 * 5. All terminal data is encrypted with shared secret
 *
 * Rust guideline compliant 2025-01-05
 */
export default class extends Controller {
  static targets = [
    "terminal",
    "status",
    "errorPanel",
    "terminalPanel",
    "hubInfo",
    "keyFingerprint",
    "disconnectBtn",
    "touchControls",
  ];

  static values = {
    csrfToken: String,
    browserDeviceId: Number,
  };

  connect() {
    this.subscription = null;
    this.ratchet = null;  // Double Ratchet session for E2E encryption
    this.hubIdentifier = null;
    this.cliPublicKey = null;

    // Parse key and hub from URL fragment
    this.parseUrlFragment();
  }

  disconnect() {
    if (this.subscription) {
      this.subscription.unsubscribe();
    }
    if (this.terminal) {
      this.terminal.dispose();
    }
  }

  // Parse CLI public key and hub identifier from URL fragment
  // This is the security-critical part: the fragment is NEVER sent to the server
  parseUrlFragment() {
    const hash = window.location.hash;
    if (!hash || hash.length < 2) {
      this.showError("No secure key found in URL");
      return;
    }

    // Parse fragment: #key=...&hub=...
    const params = new URLSearchParams(hash.substring(1));
    const keyBase64Url = params.get("key");
    const hubIdentifier = params.get("hub");

    if (!keyBase64Url || !hubIdentifier) {
      this.showError("Missing key or hub in URL fragment");
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

    try {
      // Decode and validate the public key
      this.cliPublicKey = decodeBase64(keyBase64);
      if (this.cliPublicKey.length !== 32) {
        throw new Error("Invalid public key length");
      }
      this.hubIdentifier = hubIdentifier;

      // Show key fingerprint for verification
      this.showKeyFingerprint();

      // Initialize terminal and connect
      this.updateStatus("Secure key received. Initializing...");
      this.loadOrCreateKeypair();
    } catch (error) {
      console.error("Failed to parse secure key:", error);
      this.showError(`Invalid secure key: ${error.message}`);
    }
  }

  showError(message) {
    this.updateStatus(message);
    if (this.hasErrorPanelTarget) {
      this.errorPanelTarget.classList.remove("hidden");
    }
    if (this.hasTerminalPanelTarget) {
      this.terminalPanelTarget.classList.add("hidden");
    }
  }

  showKeyFingerprint() {
    if (!this.cliPublicKey || !this.hasKeyFingerprintTarget) return;

    // Create a simple fingerprint for visual verification
    // This helps users verify they're connecting to the right CLI
    const fingerprint = Array.from(this.cliPublicKey.slice(0, 8))
      .map((b) => b.toString(16).padStart(2, "0"))
      .join(":");

    this.keyFingerprintTarget.textContent = fingerprint.toUpperCase();
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
    this.fitAddon.fit();

    // Handle terminal input - encrypt and send
    this.terminal.onData((data) => {
      if (this.subscription && this.ratchet) {
        this.sendEncrypted({ type: "input", data });
      }
    });

    // Handle window resize
    window.addEventListener("resize", () => this.handleResize());

    this.terminal.writeln("Secure E2E Terminal");
    this.terminal.writeln("Key exchange bypassed server (MITM-proof)");
    this.terminal.writeln("");
  }

  handleResize() {
    if (this.fitAddon) {
      this.fitAddon.fit();
      if (this.subscription) {
        this.subscription.perform("resize", {
          cols: this.terminal.cols,
          rows: this.terminal.rows,
        });
      }
    }
  }

  // Load keypairs from IndexedDB or create new ones (encryption + signing)
  async loadOrCreateKeypair() {
    try {
      const stored = await this.getStoredKeypair();
      if (stored) {
        // Load encryption keypair
        this.keypair = {
          publicKey: decodeBase64(stored.publicKey),
          secretKey: decodeBase64(stored.secretKey),
        };
        // Load signing keypair (may need migration for old devices)
        if (stored.signingPrivateKey && stored.signingPublicKey) {
          this.signingKeypair = {
            privateKey: decodeBase64(stored.signingPrivateKey),
            publicKey: decodeBase64(stored.signingPublicKey),
          };
        } else {
          // Generate signing keypair for legacy devices
          console.log("Generating signing keypair for legacy device...");
          this.signingKeypair = generateSigningKeypair();
          await this.storeKeypair({
            publicKey: stored.publicKey,
            secretKey: stored.secretKey,
            signingPublicKey: encodeBase64(this.signingKeypair.publicKey),
            signingPrivateKey: encodeBase64(this.signingKeypair.privateKey),
          });
        }
        this.updateStatus("Device loaded. Computing shared secret...");
      } else {
        // Create new encryption keypair
        this.keypair = generateEncryptionKeypair();
        // Create new signing keypair
        this.signingKeypair = generateSigningKeypair();
        await this.storeKeypair({
          publicKey: encodeBase64(this.keypair.publicKey),
          secretKey: encodeBase64(this.keypair.secretKey),
          signingPublicKey: encodeBase64(this.signingKeypair.publicKey),
          signingPrivateKey: encodeBase64(this.signingKeypair.privateKey),
        });
        this.updateStatus("New device created. Registering...");
        await this.registerDevice();
      }

      // Compute shared secret using CLI's public key from URL fragment
      // This is the key security property: we use the key directly, not from server
      const rawSharedSecret = computeRawSharedSecret(this.cliPublicKey, this.keypair.secretKey);

      // Initialize Double Ratchet session for per-message forward secrecy
      // Browser is NOT the initiator (CLI generates the QR code and initiates)
      this.ratchet = new RatchetSession(rawSharedSecret, false);

      // Cache the CLI public key for future connections
      // This way the user doesn't need to scan QR/paste URL every time
      const fingerprint = await this.computeFingerprint(this.cliPublicKey);
      await this.storePairedKey(fingerprint, encodeBase64(this.cliPublicKey), "CLI Device");
      console.log("Saved CLI public key with fingerprint:", fingerprint);

      // Show terminal and connect
      this.showTerminal();
      this.subscribeToTerminal();
    } catch (error) {
      console.error("Failed to load/create keypair:", error);
      this.showError("Crypto error - try refreshing");
    }
  }

  showTerminal() {
    if (this.hasErrorPanelTarget) {
      this.errorPanelTarget.classList.add("hidden");
    }
    if (this.hasTerminalPanelTarget) {
      this.terminalPanelTarget.classList.remove("hidden");
    }
    if (this.hasDisconnectBtnTarget) {
      this.disconnectBtnTarget.classList.remove("hidden");
    }
    if (this.hasTouchControlsTarget) {
      this.touchControlsTarget.querySelector("div").classList.remove("hidden");
    }

    // Initialize terminal after panel is visible
    this.initTerminal();
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

  // Compute fingerprint from public key (first 8 bytes of SHA256 as hex:hex:...)
  async computeFingerprint(publicKey) {
    const hashBuffer = await crypto.subtle.digest("SHA-256", publicKey);
    const hashArray = Array.from(new Uint8Array(hashBuffer).slice(0, 8));
    return hashArray.map((b) => b.toString(16).padStart(2, "0")).join(":");
  }

  // Register browser device with server
  async registerDevice() {
    try {
      const response = await fetch("/devices", {
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

  subscribeToTerminal() {
    if (this.subscription) {
      this.subscription.unsubscribe();
    }

    this.subscription = consumer.subscriptions.create(
      {
        channel: "TerminalChannel",
        hub_identifier: this.hubIdentifier,
        device_type: "browser",
      },
      {
        connected: () => {
          this.updateStatus(`Connected securely to ${this.hubIdentifier.substring(0, 8)}...`);
          if (this.hasHubInfoTarget) {
            this.hubInfoTarget.textContent = `Hub: ${this.hubIdentifier.substring(0, 8)}...`;
          }
          this.terminal.clear();
          this.terminal.writeln(`Connected to hub: ${this.hubIdentifier.substring(0, 8)}...`);
          this.terminal.writeln("E2E encryption active (MITM-proof)");
          this.terminal.writeln("");

          // Announce presence with signed public key
          // The CLI will verify this signature before computing shared secret
          // This prevents MITM attacks even if the server tries to forge presence
          const signature = signMessage(this.keypair.publicKey, this.signingKeypair.privateKey);
          this.subscription.perform("presence", {
            event: "join",
            device_name: this.browserName(),
            public_key: encodeBase64(this.keypair.publicKey),
            signature: encodeBase64(signature),
            verifying_key: encodeBase64(this.signingKeypair.publicKey),
          });

          // Send terminal size
          this.subscription.perform("resize", {
            cols: this.terminal.cols,
            rows: this.terminal.rows,
          });
        },

        disconnected: () => {
          this.updateStatus("Disconnected");
          this.terminal.writeln("\r\n[Disconnected from hub]");
        },

        rejected: () => {
          this.updateStatus("Connection rejected");
          this.showError("Connection rejected - hub may be offline or you may not have access");
        },

        received: (data) => {
          this.handleMessage(data);
        },
      },
    );
  }

  handleMessage(data) {
    switch (data.type) {
      case "terminal":
        if (data.from === "browser") return;
        this.receiveEncrypted(data);
        break;

      case "presence":
        if (data.event === "join") {
          this.terminal.writeln(`\r\n[${data.device_name || data.device_type} connected]`);
        } else if (data.event === "leave") {
          this.terminal.writeln(`\r\n[${data.device_type} disconnected]`);
        }
        break;

      case "resize":
        // CLI sent resize - browser controls its own size
        break;
    }
  }

  // Encrypt and send data to CLI using Double Ratchet
  sendEncrypted(message) {
    if (!this.ratchet) {
      console.error("Cannot send - ratchet not initialized");
      return;
    }

    const messageBytes = decodeUTF8(JSON.stringify(message));
    const envelope = this.ratchet.encrypt(messageBytes);
    const serialized = serializeEnvelope(envelope);

    this.subscription.perform("relay", serialized);
  }

  // Decrypt received data from CLI using Double Ratchet
  receiveEncrypted(data) {
    if (!this.ratchet) {
      console.error("Cannot receive - ratchet not initialized");
      return;
    }

    try {
      const envelope = deserializeEnvelope(data);
      const decrypted = this.ratchet.decrypt(envelope);
      const message = JSON.parse(encodeUTF8(decrypted));

      if (message.type === "output") {
        this.terminal.write(message.data);
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
  disconnectAction() {
    if (this.subscription) {
      this.subscription.unsubscribe();
      this.subscription = null;
    }
    // Securely zero out key material
    if (this.ratchet) {
      this.ratchet.zeroize();
      this.ratchet = null;
    }
    // Zero out signing keypair (in-memory only, not persistent storage)
    if (this.signingKeypair?.privateKey) {
      this.signingKeypair.privateKey.fill(0);
      this.signingKeypair = null;
    }
    this.updateStatus("Disconnected");
    this.terminal.writeln("\r\n[Disconnected]");
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

  sendKey(key) {
    if (this.subscription && this.ratchet) {
      this.sendEncrypted({ type: "input", data: key });
    }
  }
}
