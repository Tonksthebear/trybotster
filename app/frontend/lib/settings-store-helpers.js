import { queryClient } from './query-client'
import { queryKeys } from './queries'

export function settingsFsContext(configScope, selectedTargetId) {
  return {
    fs: configScope === 'device' ? 'device' : 'repo',
    tid: configScope === 'repo' ? selectedTargetId : undefined,
    prefix: configScope === 'device' ? '' : '.botster/',
  }
}

export function defaultSettingsContent(filePath, configMetadata = {}) {
  if (filePath.endsWith('/initialization')) {
    return configMetadata.session_files?.initialization?.default || '#!/bin/bash\n'
  }
  if (filePath.endsWith('.json')) {
    return '{\n  "agents": [],\n  "accessories": []\n}\n'
  }
  return ''
}

export function invalidateAgentConfigQueries({
  configScope,
  hubId,
  spawnTargets = [],
  selectedTargetId,
}) {
  if (!hubId) return

  const targetIds =
    configScope === 'repo'
      ? [selectedTargetId]
      : spawnTargets.map((target) => target?.id)

  targetIds.filter(Boolean).forEach((targetId) => {
    queryClient.invalidateQueries({
      queryKey: queryKeys.agentConfig(hubId, targetId),
    })
  })
}

export async function scanSettingsTree({ hub, configScope, selectedTargetId }) {
  const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)
  const tree = { agents: {}, accessories: {}, workspaces: {}, plugins: {} }

  if (configScope === 'device') {
    const [agents, accessories, plugins, workspaces] = await Promise.all([
      hub.statFile('agents', fs, tid).catch(() => ({ exists: false })),
      hub.statFile('accessories', fs, tid).catch(() => ({ exists: false })),
      hub.statFile('plugins', fs, tid).catch(() => ({ exists: false })),
      hub.statFile('workspaces', fs, tid).catch(() => ({ exists: false })),
    ])
    if (!agents.exists && !accessories.exists && !plugins.exists && !workspaces.exists) {
      return { state: 'empty', tree: null }
    }
  } else {
    const stat = await hub.statFile('.botster', fs, tid).catch(() => ({ exists: false }))
    if (!stat.exists) return { state: 'empty', tree: null }
  }

  const listDirs = async (path) => {
    try {
      const result = await hub.listDir(path, fs, tid)
      return (result.entries || [])
        .filter((entry) => entry.type === 'dir')
        .map((entry) => entry.name)
        .sort()
    } catch {
      return []
    }
  }

  const listFiles = async (path, ext) => {
    try {
      const result = await hub.listDir(path, fs, tid)
      return (result.entries || [])
        .filter((entry) => entry.type === 'file' && (!ext || entry.name.endsWith(ext)))
        .map((entry) => entry.name)
        .sort()
    } catch {
      return []
    }
  }

  const [agentNames, accessoryNames, workspaceEntries, pluginNames] =
    await Promise.all([
      listDirs(`${prefix}agents`),
      listDirs(`${prefix}accessories`),
      listFiles(`${prefix}workspaces`, '.json'),
      listDirs(`${prefix}plugins`),
    ])

  await Promise.all(
    agentNames.map(async (name) => {
      const initStat = await hub
        .statFile(`${prefix}agents/${name}/initialization`, fs, tid)
        .catch(() => ({ exists: false }))
      tree.agents[name] = { initialization: initStat.exists }
    })
  )

  await Promise.all(
    accessoryNames.map(async (name) => {
      const path = `${prefix}accessories/${name}`
      const [initStat, portForwardStat] = await Promise.all([
        hub.statFile(`${path}/initialization`, fs, tid).catch(() => ({ exists: false })),
        hub.statFile(`${path}/port_forward`, fs, tid).catch(() => ({ exists: false })),
      ])
      tree.accessories[name] = {
        initialization: initStat.exists,
        port_forward: portForwardStat.exists,
      }
    })
  )

  workspaceEntries.forEach((fileName) => {
    tree.workspaces[fileName.replace(/\.json$/, '')] = {
      file: `${prefix}workspaces/${fileName}`,
    }
  })

  await Promise.all(
    pluginNames.map(async (name) => {
      const initStat = await hub
        .statFile(`${prefix}plugins/${name}/init.lua`, fs, tid)
        .catch(() => ({ exists: false }))
      if (initStat.exists) tree.plugins[name] = { init: true }
    })
  )

  return { state: 'tree', tree }
}

export function pluginName(dest) {
  const match = dest?.match(/plugins\/([^/]+)\//)
  return match ? match[1] : dest
}
