import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"

const mocks = vi.hoisted(() => {
  const subscriptions = {
    create: vi.fn(() => ({
      perform: vi.fn(),
      unsubscribe: vi.fn(),
    })),
  }

  return {
    subscriptions,
    createConsumer: vi.fn(() => ({
      connection: {
        webSocket: {},
        isOpen: () => true,
        isActive: () => true,
        open: vi.fn(),
        close: vi.fn(),
        installEventHandlers: vi.fn(),
      },
      subscriptions,
    })),
    bridge: {
      createSession: vi.fn(),
      decryptBinary: vi.fn(),
      encryptBinary: vi.fn(),
      decrypt: vi.fn(),
      encrypt: vi.fn(() => Promise.resolve({ encrypted: { t: 1, b: "signal" } })),
    },
  }
})

vi.mock("@rails/actioncable", () => ({
  createConsumer: mocks.createConsumer,
}))

vi.mock("workers/bridge", () => ({
  default: mocks.bridge,
}))

class MockDataChannel {
  constructor() {
    this.readyState = "connecting"
    this.binaryType = ""
    this.onopen = null
    this.onclose = null
    this.onerror = null
    this.onmessage = null
    this.send = vi.fn()
  }
}

class MockRTCPeerConnection {
  static instances = []

  constructor() {
    this.connectionState = "new"
    this.iceConnectionState = "new"
    this.onicecandidate = null
    this.oniceconnectionstatechange = null
    this.onconnectionstatechange = null
    this.dataChannel = new MockDataChannel()
    this.close = vi.fn(() => {
      this.connectionState = "closed"
    })
    this.createDataChannel = vi.fn(() => this.dataChannel)
    this.createOffer = vi.fn(() => Promise.resolve({ type: "offer", sdp: "offer-sdp" }))
    this.setLocalDescription = vi.fn(() => Promise.resolve())
    this.setRemoteDescription = vi.fn(() => Promise.resolve())
    this.addIceCandidate = vi.fn(() => Promise.resolve())
    this.restartIce = vi.fn()
    this.getStats = vi.fn(() => Promise.resolve(new Map()))
    MockRTCPeerConnection.instances.push(this)
  }
}

async function flushPromises() {
  await Promise.resolve()
  await Promise.resolve()
}

describe("HubPeerConnection peer lost transitions", () => {
  let HubPeerConnection
  let transport
  let events

  beforeEach(async () => {
    vi.useFakeTimers()
    vi.spyOn(console, "debug").mockImplementation(() => {})
    vi.spyOn(console, "error").mockImplementation(() => {})
    vi.spyOn(console, "warn").mockImplementation(() => {})
    MockRTCPeerConnection.instances = []
    globalThis.RTCPeerConnection = MockRTCPeerConnection
    globalThis.RTCSessionDescription = class {
      constructor(description) {
        Object.assign(this, description)
      }
    }
    globalThis.RTCIceCandidate = class {
      constructor(candidate) {
        Object.assign(this, candidate)
      }
    }
    globalThis.fetch = vi.fn(() => Promise.resolve({
      ok: true,
      json: () => Promise.resolve({ ice_servers: [] }),
    }))
    mocks.subscriptions.create.mockClear()
    mocks.bridge.encrypt.mockClear()

    ;({ HubPeerConnection } = await import("../lib/transport/hub_peer_connection"))
    transport = new HubPeerConnection()
    events = []
    transport.on("connection:state", (event) => events.push(event))
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.restoreAllMocks()
    delete globalThis.RTCPeerConnection
    delete globalThis.RTCSessionDescription
    delete globalThis.RTCIceCandidate
    delete globalThis.fetch
  })

  async function connectPeer(hubId = "hub-1") {
    await transport.connectSignaling(hubId, "browser-identity")
    await transport.connectPeer(hubId)
    return MockRTCPeerConnection.instances.at(-1)
  }

  function disconnectedEvents() {
    return events.filter((event) => event.state === "disconnected")
  }

  function connectedEvents() {
    return events.filter((event) => event.state === "connected")
  }

  it("emits peer-ready timing when the data channel opens", async () => {
    const pc = await connectPeer()

    pc.dataChannel.onopen()

    expect(connectedEvents()).toHaveLength(1)
    expect(connectedEvents()[0]).toMatchObject({
      hubId: "hub-1",
      state: "connected",
      peerReadyMs: expect.any(Number),
    })
    expect(connectedEvents()[0].peerReadyMs).toBeGreaterThanOrEqual(0)
  })

  it("emits one datachannel_close event and tears down peer timers", async () => {
    const pc = await connectPeer()
    const dc = pc.dataChannel

    dc.onclose()

    expect(disconnectedEvents()).toEqual([
      { hubId: "hub-1", state: "disconnected", reason: "datachannel_close" },
    ])
    expect(pc.close).toHaveBeenCalledTimes(1)
    expect(pc.onconnectionstatechange).toBeNull()
    expect(dc.onclose).toBeNull()
    expect(vi.getTimerCount()).toBe(0)
  })

  it("emits once when onclose is followed by pc closed", async () => {
    const pc = await connectPeer()
    const onConnectionStateChange = pc.onconnectionstatechange

    pc.dataChannel.onclose()
    pc.connectionState = "closed"
    onConnectionStateChange()

    expect(disconnectedEvents()).toHaveLength(1)
    expect(disconnectedEvents()[0].reason).toBe("datachannel_close")
  })

  it("emits once when onerror is followed by onclose", async () => {
    const pc = await connectPeer()
    const onclose = pc.dataChannel.onclose

    pc.dataChannel.onerror(new Error("boom"))
    onclose()

    expect(disconnectedEvents()).toHaveLength(1)
    expect(disconnectedEvents()[0].reason).toBe("datachannel_error")
  })

  it("peer setup timer emits peer_setup_timeout and clears peer connect promise", async () => {
    await transport.connectSignaling("hub-1", "browser-identity")
    const firstConnect = transport.connectPeer("hub-1")
    await flushPromises()
    const firstPc = MockRTCPeerConnection.instances.at(-1)

    await vi.advanceTimersByTimeAsync(15_000)
    await expect(firstConnect).resolves.toEqual({ state: "connecting" })

    expect(disconnectedEvents()).toEqual([
      { hubId: "hub-1", state: "disconnected", reason: "peer_setup_timeout" },
    ])

    await transport.connectPeer("hub-1")
    expect(MockRTCPeerConnection.instances.at(-1)).not.toBe(firstPc)
  })

  it("probePeerHealth cleans failed pc, and later disconnectPeer emits no second event", async () => {
    const pc = await connectPeer()
    pc.connectionState = "failed"

    expect(transport.probePeerHealth("hub-1")).toEqual({
      alive: false,
      pcState: "failed",
      dcState: "connecting",
    })
    transport.disconnectPeer("hub-1")

    expect(disconnectedEvents()).toHaveLength(1)
    expect(disconnectedEvents()[0].reason).toBe("probe_dead")
  })

  it("explicit disconnectPeer emits explicit_disconnect", async () => {
    await connectPeer()

    transport.disconnectPeer("hub-1")

    expect(disconnectedEvents()).toEqual([
      { hubId: "hub-1", state: "disconnected", reason: "explicit_disconnect" },
    ])
  })

  it("keeps signaling and connection entry after peer lost, then starts a fresh peer attempt", async () => {
    const pc = await connectPeer()
    const subscription = mocks.subscriptions.create.mock.results[0].value

    pc.dataChannel.onclose()
    await transport.connectPeer("hub-1")

    expect(subscription.unsubscribe).not.toHaveBeenCalled()
    expect(MockRTCPeerConnection.instances).toHaveLength(2)
    expect(disconnectedEvents()).toHaveLength(1)
  })

  it("clears a stale peer on connect and starts a fresh attempt", async () => {
    const pc = await connectPeer()
    pc.connectionState = "failed"

    await transport.connectPeer("hub-1")

    expect(disconnectedEvents()).toEqual([
      { hubId: "hub-1", state: "disconnected", reason: "stale_peer_on_connect" },
    ])
    expect(MockRTCPeerConnection.instances).toHaveLength(2)
    expect(MockRTCPeerConnection.instances.at(-1)).not.toBe(pc)
  })

  it("ICE stuck disconnected emits ice_disconnect_stuck", async () => {
    const pc = await connectPeer()

    pc.iceConnectionState = "disconnected"
    pc.oniceconnectionstatechange()
    await vi.advanceTimersByTimeAsync(5_000)

    expect(disconnectedEvents()).toEqual([
      { hubId: "hub-1", state: "disconnected", reason: "ice_disconnect_stuck" },
    ])
  })
})
