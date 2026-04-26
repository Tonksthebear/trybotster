import { describe, it, expect } from 'vitest'
import { resolveHubStatus } from '../lib/connections/hub_connection_status'

describe('resolveHubStatus', () => {
  it('returns "offline" when transport reports offline, even if entity is ready', () => {
    // Paired hub that lost transport — server-side health beats local liveness
    // so the user sees the dot reflect the real reachability.
    expect(resolveHubStatus('offline', true)).toBe('offline')
    expect(resolveHubStatus('offline', false)).toBe('offline')
  })

  it('returns "online" when transport reports online', () => {
    expect(resolveHubStatus('online', false)).toBe('online')
    expect(resolveHubStatus('online', true)).toBe('online')
  })

  it('returns "online" when entity is ready and transport has no opinion', () => {
    // Fresh / unpaired hub: no health events yet, but the local hub_recovery_state
    // reached "ready" — the user mental model is "local hub up = green".
    expect(resolveHubStatus(null, true)).toBe('online')
    expect(resolveHubStatus(undefined, true)).toBe('online')
  })

  it('returns null when transport is silent and entity is not ready', () => {
    // Render layer maps null → "connecting" amber dot.
    expect(resolveHubStatus(null, false)).toBeNull()
    expect(resolveHubStatus(undefined, undefined)).toBeNull()
  })
})
