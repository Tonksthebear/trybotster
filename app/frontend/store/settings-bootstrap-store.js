import { create } from 'zustand'

const settingsBootstrapCache = new Map()

export const useSettingsBootstrapStore = create((set, get) => ({
  hubId: null,
  data: null,
  loading: false,

  async load(hubId) {
    if (!hubId) return null
    const key = String(hubId)
    const cached = settingsBootstrapCache.get(key) || null
    set({
      hubId: key,
      data: cached,
      loading: cached ? false : true,
    })

    try {
      const res = await fetch(`/hubs/${key}/settings.json`, {
        headers: { Accept: 'application/json' },
        credentials: 'same-origin',
      })
      if (!res.ok) throw new Error(`${res.status}`)
      const data = await res.json()
      settingsBootstrapCache.set(key, data)
      if (get().hubId === key) {
        set({ data, loading: false })
      }
      return data
    } catch (err) {
      console.warn('[settings-bootstrap-store] Failed to fetch settings bootstrap:', err)
      if (get().hubId === key) {
        set({
          data: settingsBootstrapCache.get(key) || {},
          loading: false,
        })
      }
      return settingsBootstrapCache.get(key) || null
    }
  },
}))

export function resetSettingsBootstrapCacheForTests() {
  settingsBootstrapCache.clear()
  useSettingsBootstrapStore.setState({
    hubId: null,
    data: null,
    loading: false,
  })
}
