/** Main-thread bridge for the WebRTC browser transport. */

let instance = null
let webrtcTransport = null

const FORWARDED_EVENTS = [
  "connection:state", "connection:mode", "connection:stalled",
  "subscription:message", "subscription:confirmed", "subscription:ready",
  "health", "browser:state", "signaling:state", "stream:frame",
  "push:status", "push:vapid_key", "push:sub_ack", "push:vapid_keys",
  "push:test_ack", "push:disable_ack",
]

class WorkerBridge {
  #initialized = false
  #initPromise = null
  #eventListeners = new Map()
  #subscriptionListeners = new Map()

  static get instance() {
    if (!instance) instance = new WorkerBridge()
    return instance
  }

  async init() {
    if (this.#initialized) return
    if (this.#initPromise) return this.#initPromise
    this.#initPromise = this.#doInit()
    return this.#initPromise
  }

  async #doInit() {
    try {
      const { default: transport } = await import("transport/hub_peer_connection")
      webrtcTransport = transport
      for (const event of FORWARDED_EVENTS) {
        transport.on(event, (data) => this.#dispatchEvent({ event, ...data }))
      }
      this.#initialized = true
    } catch (error) {
      this.#initPromise = null
      throw error
    }
  }

  #dispatchEvent(data) {
    for (const callback of this.#eventListeners.get(data.event) || []) callback(data)
    if (data.event === "subscription:message" && data.subscriptionId) {
      for (const callback of this.#subscriptionListeners.get(data.subscriptionId) || []) {
        callback(data.message)
      }
    }
  }

  async send(action, params = {}) {
    if (!webrtcTransport) throw new Error("WebRTC transport not initialized")
    switch (action) {
      case "connect": return webrtcTransport.connect(params.hubId, params.browserIdentity)
      case "connectSignaling": return webrtcTransport.connectSignaling(params.hubId, params.browserIdentity)
      case "connectPeer": return webrtcTransport.connectPeer(params.hubId)
      case "disconnectPeer": return webrtcTransport.disconnectPeer(params.hubId)
      case "probePeerHealth": return webrtcTransport.probePeerHealth(params.hubId)
      case "disconnect": return webrtcTransport.disconnect(params.hubId)
      case "subscribe": {
        const payload = new TextEncoder().encode(JSON.stringify({
          type: "subscribe", subscriptionId: params.subscriptionId,
          channel: params.channel, params: params.params,
        }))
        const frame = new Uint8Array(1 + payload.length)
        frame[0] = 0x00
        frame.set(payload, 1)
        return webrtcTransport.subscribe(params.hubId, params.channel, params.params, params.subscriptionId, frame)
      }
      case "unsubscribe": return webrtcTransport.unsubscribe(params.subscriptionId)
      case "sendRaw": return webrtcTransport.sendRaw(params.subscriptionId, params.message)
      case "sendData": return webrtcTransport.sendData(params.hubId, params.plaintext)
      case "sendStreamFrame": return webrtcTransport.sendStreamFrame(params.hubId, params.frameType, params.streamId, params.payload)
      case "sendPtyInput": return webrtcTransport.sendPtyInput(params.hubId, params.subscriptionId, params.data)
      case "sendFileInput": return webrtcTransport.sendFileInput(params.hubId, params.subscriptionId, params.data, params.filename)
      case "sendControlMessage": {
        const payload = new TextEncoder().encode(JSON.stringify(params.message))
        const frame = new Uint8Array(1 + payload.length)
        frame[0] = 0x00
        frame.set(payload, 1)
        return webrtcTransport.sendData(params.hubId, frame)
      }
      default: throw new Error(`Unknown action: ${action}`)
    }
  }

  on(eventName, callback) {
    if (!this.#eventListeners.has(eventName)) this.#eventListeners.set(eventName, new Set())
    this.#eventListeners.get(eventName).add(callback)
    return () => {
      const listeners = this.#eventListeners.get(eventName)
      listeners?.delete(callback)
      if (listeners?.size === 0) this.#eventListeners.delete(eventName)
    }
  }

  onSubscriptionMessage(subscriptionId, callback) {
    if (!this.#subscriptionListeners.has(subscriptionId)) {
      this.#subscriptionListeners.set(subscriptionId, new Set())
    }
    this.#subscriptionListeners.get(subscriptionId).add(callback)
    return () => this.#subscriptionListeners.get(subscriptionId)?.delete(callback)
  }

  clearSubscriptionListeners(subscriptionId) {
    this.#subscriptionListeners.delete(subscriptionId)
    webrtcTransport?.clearSubscriptionListeners(subscriptionId)
  }

  get isInitialized() { return this.#initialized }
}

export { WorkerBridge }
export default WorkerBridge.instance
