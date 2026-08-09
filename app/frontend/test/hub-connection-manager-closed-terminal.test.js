import { describe, expect, it, vi } from 'vitest'

import { HubConnectionManager } from '../lib/connections/hub_connection_manager'

class ClosedTerminalStub {
  constructor(key, options, manager) {
    this.key = key
    this.options = options
    this.manager = manager
    this.state = 'disconnected'
    this.lastError = null
    this._closed = true
    this.destroy = vi.fn(() => {
      this._closed = true
    })
    this.reacquire = vi.fn(async () => {})
    this.notifyIdle = vi.fn()
    this.getHubId = () => options.hubId
    this.isHubConnected = () => false
  }

  isSessionClosed() {
    return this._closed
  }

  release() {
    this.manager.release(this.key)
  }
}

class OpenTerminalStub {
  constructor(key, options, manager) {
    this.key = key
    this.options = options
    this.manager = manager
    this.state = 'connected'
    this.lastError = null
    this.destroy = vi.fn()
    this.reacquire = vi.fn(async () => {})
    this.notifyIdle = vi.fn()
    this.initialize = vi.fn(async () => {})
    this.getHubId = () => options.hubId
    this.isHubConnected = () => false
  }

  isSessionClosed() {
    return false
  }

  release() {
    this.manager.release(this.key)
  }
}

describe('HubConnectionManager closed terminal reuse', () => {
  it('destroys permanently closed terminal wrappers and creates a fresh one', async () => {
    const key = 'terminal:hub-1:session-1'
    const closed = new ClosedTerminalStub(key, { hubId: 'hub-1' }, HubConnectionManager)
    // Seed the pool with a closed terminal the way process_exited leaves it.
    HubConnectionManager.connections.set(key, { wrapper: closed, refCount: 0 })

    OpenTerminalStub.prototype.initialize = async function initialize() {}
    const fresh = await HubConnectionManager.acquire(OpenTerminalStub, key, {
      hubId: 'hub-1',
      sessionUuid: 'session-1',
    })

    expect(closed.destroy).toHaveBeenCalled()
    expect(fresh).toBeInstanceOf(OpenTerminalStub)
    expect(fresh.isSessionClosed()).toBe(false)

    HubConnectionManager.destroy(key)
  })
})
