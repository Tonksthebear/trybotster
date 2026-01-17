/**
 * Preview Service Worker - HTTP Tunnel via E2E Encrypted Channel
 *
 * This service worker intercepts fetch requests from preview iframes
 * and routes them through the encrypted PreviewChannel to the agent's
 * dev server.
 *
 * Architecture:
 * 1. Service worker intercepts fetch from preview iframe
 * 2. Sends request to main thread via MessageChannel
 * 3. Main thread encrypts and sends via PreviewChannel WebSocket
 * 4. Agent's dev server processes request, returns encrypted response
 * 5. Main thread decrypts and sends back to service worker
 * 6. Service worker returns response to fetch
 *
 * This design keeps Signal Protocol session in main thread (security)
 * while enabling transparent HTTP proxying.
 */

// Map of active message channels to clients
const clientChannels = new Map();

// Request timeout (ms)
const REQUEST_TIMEOUT = 30000;

/**
 * Install event - immediate activation
 */
self.addEventListener("install", (event) => {
  console.log("[PreviewSW] Installing...");
  // Skip waiting to activate immediately
  self.skipWaiting();
});

/**
 * Activate event - claim all clients
 */
self.addEventListener("activate", (event) => {
  console.log("[PreviewSW] Activating...");
  event.waitUntil(self.clients.claim());
});

/**
 * Message handler - receive MessagePort from main thread
 */
self.addEventListener("message", (event) => {
  const { type, clientId, port } = event.data;

  if (type === "connect" && port) {
    console.log("[PreviewSW] Client connected:", clientId);
    clientChannels.set(clientId, {
      port,
      pendingRequests: new Map(),
      nextRequestId: 1,
    });

    // Listen for responses on this port
    port.onmessage = (msg) => handlePortMessage(clientId, msg.data);
    port.start();

    // Acknowledge connection
    port.postMessage({ type: "connected" });
  }

  if (type === "disconnect") {
    console.log("[PreviewSW] Client disconnected:", clientId);
    const client = clientChannels.get(clientId);
    if (client) {
      // Reject all pending requests
      for (const [, pending] of client.pendingRequests) {
        pending.reject(new Error("Channel disconnected"));
      }
      clientChannels.delete(clientId);
    }
  }
});

/**
 * Handle response messages from main thread
 */
function handlePortMessage(clientId, data) {
  const client = clientChannels.get(clientId);
  if (!client) return;

  if (data.type === "http_response") {
    const pending = client.pendingRequests.get(data.request_id);
    if (pending) {
      client.pendingRequests.delete(data.request_id);
      clearTimeout(pending.timer);
      pending.resolve(data);
    }
  }

  if (data.type === "http_error") {
    const pending = client.pendingRequests.get(data.request_id);
    if (pending) {
      client.pendingRequests.delete(data.request_id);
      clearTimeout(pending.timer);
      pending.reject(new Error(data.error));
    }
  }
}

/**
 * Fetch event - intercept and proxy requests
 */
self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Only intercept requests that should be proxied
  // Check for preview scope marker or specific patterns
  if (!shouldProxy(event.request, url)) {
    return; // Let browser handle normally
  }

  event.respondWith(handleProxiedFetch(event));
});

/**
 * Determine if a request should be proxied through the tunnel.
 *
 * Proxy rules:
 * - Requests from pages with preview scope
 * - Requests to localhost ports (agent dev server)
 * - NOT requests to external domains
 * - NOT requests to the Rails server itself
 */
function shouldProxy(request, url) {
  // Check referrer or client for preview context
  const referrer = request.referrer;

  // If referrer contains /preview/, this is a preview iframe request
  if (referrer && referrer.includes("/preview/")) {
    // Proxy localhost requests (dev server)
    if (url.hostname === "localhost" || url.hostname === "127.0.0.1") {
      return true;
    }
  }

  // Check request mode - navigation requests in preview iframe
  if (request.mode === "navigate") {
    // Check if this is within our preview scope
    // The preview controller will set a specific header or use a specific path
    if (url.pathname.startsWith("/__preview__/")) {
      return true;
    }
  }

  return false;
}

/**
 * Handle a proxied fetch request.
 */
async function handleProxiedFetch(event) {
  const clientId = event.clientId || event.resultingClientId;

  // Get the channel for this client
  const client = clientChannels.get(clientId);

  if (!client) {
    // No channel - client hasn't connected yet
    // Return an error page that instructs the user
    return new Response(
      `<!DOCTYPE html>
<html>
<head><title>Preview Not Connected</title></head>
<body style="font-family: system-ui; padding: 2rem; background: #111; color: #fff;">
  <h1>Preview Not Connected</h1>
  <p>The preview channel is not connected. Please ensure:</p>
  <ul>
    <li>You have scanned the QR code to establish encryption</li>
    <li>The agent's dev server is running</li>
    <li>The preview page has loaded completely</li>
  </ul>
  <p><a href="javascript:location.reload()" style="color: #0af;">Reload to retry</a></p>
</body>
</html>`,
      {
        status: 503,
        statusText: "Service Unavailable",
        headers: { "Content-Type": "text/html" },
      }
    );
  }

  try {
    const response = await proxyRequest(event.request, client);
    return response;
  } catch (error) {
    console.error("[PreviewSW] Proxy error:", error);
    return new Response(
      JSON.stringify({ error: error.message }),
      {
        status: 502,
        statusText: "Bad Gateway",
        headers: { "Content-Type": "application/json" },
      }
    );
  }
}

/**
 * Proxy a request through the encrypted channel.
 */
async function proxyRequest(request, client) {
  const requestId = client.nextRequestId++;
  const url = new URL(request.url);

  // Collect request data
  const method = request.method;
  const headers = {};
  for (const [key, value] of request.headers) {
    headers[key] = value;
  }

  // Read body if present
  let body = null;
  if (request.body) {
    const buffer = await request.arrayBuffer();
    body = arrayBufferToBase64(buffer);
  }

  // Create promise for response
  const responsePromise = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      client.pendingRequests.delete(requestId);
      reject(new Error("Request timeout"));
    }, REQUEST_TIMEOUT);

    client.pendingRequests.set(requestId, { resolve, reject, timer });
  });

  // Send request to main thread
  client.port.postMessage({
    type: "http_request",
    request_id: requestId,
    method,
    url: url.pathname + url.search,
    headers,
    body,
  });

  // Wait for response
  const response = await responsePromise;

  // Build Response object
  const responseHeaders = new Headers();
  if (response.headers) {
    for (const [key, value] of Object.entries(response.headers)) {
      // Skip hop-by-hop headers
      if (!isHopByHopHeader(key)) {
        responseHeaders.set(key, value);
      }
    }
  }

  // Decode body
  let responseBody = null;
  if (response.body) {
    responseBody = base64ToArrayBuffer(response.body);
  }

  return new Response(responseBody, {
    status: response.status || 200,
    statusText: response.status_text || "OK",
    headers: responseHeaders,
  });
}

/**
 * Check if header is hop-by-hop (should not be forwarded).
 */
function isHopByHopHeader(name) {
  const hopByHop = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
  ];
  return hopByHop.includes(name.toLowerCase());
}

/**
 * Convert ArrayBuffer to base64 string.
 */
function arrayBufferToBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

/**
 * Convert base64 string to ArrayBuffer.
 */
function base64ToArrayBuffer(base64) {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}
