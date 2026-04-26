import { useQuery } from '@tanstack/react-query'
import { waitForHub } from './hub-bridge'

export const queryKeys = {
  hubList: () => ['hubs'],
  settingsBootstrap: (hubId) => ['hub', String(hubId), 'settingsBootstrap'],
  agentConfig: (hubId, targetId) => ['hub', String(hubId), 'agentConfig', String(targetId)],
}

export async function fetchHubList() {
  const res = await fetch('/hubs.json', {
    headers: { Accept: 'application/json' },
    credentials: 'same-origin',
  })
  if (res.status === 401 || res.redirected) {
    window.location.href = '/github/authorization/new'
    return []
  }
  if (!res.ok) throw new Error(`${res.status}`)
  const data = await res.json()
  return Array.isArray(data) ? data : data.hubs || []
}

export function useHubListQuery() {
  return useQuery({
    queryKey: queryKeys.hubList(),
    queryFn: fetchHubList,
    staleTime: 30_000,
  })
}

export async function fetchSettingsBootstrap(hubId) {
  if (!hubId) return null
  const key = String(hubId)
  const res = await fetch(`/hubs/${key}/settings.json`, {
    headers: { Accept: 'application/json' },
    credentials: 'same-origin',
  })
  if (!res.ok) throw new Error(`${res.status}`)
  return res.json()
}

export function useSettingsBootstrapQuery(hubId) {
  return useQuery({
    queryKey: queryKeys.settingsBootstrap(hubId),
    enabled: !!hubId,
    queryFn: () => fetchSettingsBootstrap(hubId),
    staleTime: 60_000,
  })
}

export async function fetchAgentConfig(hubId, targetId, options = {}) {
  if (!hubId || !targetId) {
    return { targetId: targetId || null, agents: [], accessories: [], workspaces: [] }
  }
  const hub = await waitForHub(hubId)
  if (!hub) throw new Error('Hub connection is not ready')
  return hub.ensureAgentConfig(targetId, options)
}

export function agentConfigQueryOptions(hubId, targetId, options = {}) {
  return {
    queryKey: queryKeys.agentConfig(hubId, targetId),
    enabled: options.enabled ?? (!!hubId && !!targetId),
    queryFn: () => fetchAgentConfig(hubId, targetId, options.force ? { force: true } : {}),
    staleTime: options.staleTime ?? 30_000,
  }
}

export function useAgentConfigQuery(hubId, targetId, options = {}) {
  return useQuery(agentConfigQueryOptions(hubId, targetId, options))
}
