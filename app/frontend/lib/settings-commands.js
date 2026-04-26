import {
  defaultSettingsContent,
  settingsFsContext,
} from './settings-store-helpers'

export async function readSettingsFile({ hub, configScope, selectedTargetId, filePath }) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
  const stat = await hub.statFile(filePath, fs, tid)
  if (!stat.exists) return { exists: false, content: null }

  const result = await hub.readFile(filePath, fs, tid)
  return { exists: true, content: result.content }
}

export async function writeSettingsFile({
  hub,
  configScope,
  selectedTargetId,
  filePath,
  content,
}) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
  await hub.writeFile(filePath, content, fs, tid)
}

export async function createSettingsFile({
  hub,
  configMetadata,
  configScope,
  selectedTargetId,
  filePath,
  content,
}) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
  const nextContent = content || defaultSettingsContent(filePath, configMetadata)
  const parentDir = filePath.replace(/\/[^/]+$/, '')

  await hub.mkDir(parentDir, fs, tid).catch(() => {})
  await hub.writeFile(filePath, nextContent, fs, tid)

  return nextContent
}

export async function deleteSettingsFile({ hub, configScope, selectedTargetId, filePath }) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
  await hub.deleteFile(filePath, fs, tid)
}

export async function renameSettingsFile({
  hub,
  configScope,
  selectedTargetId,
  filePath,
  newPath,
}) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)
  await hub.renameFile(filePath, newPath, fs, tid)
}

export async function setPortForward({
  hub,
  configScope,
  selectedTargetId,
  filePath,
  enabled,
}) {
  const { fs, tid } = settingsFsContext(configScope, selectedTargetId)

  if (enabled) {
    const parentDir = filePath.replace(/\/[^/]+$/, '')
    await hub.mkDir(parentDir, fs, tid).catch(() => {})
    await hub.writeFile(filePath, '', fs, tid)
  } else {
    await hub.deleteFile(filePath, fs, tid)
  }
}

export async function createSessionConfig({
  hub,
  configMetadata,
  configScope,
  selectedTargetId,
  type,
  name,
}) {
  const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)
  const basePath = `${prefix}${type}/${name}`
  const defaultInit = defaultSettingsContent(`${basePath}/initialization`, configMetadata)

  await hub.mkDir(basePath, fs, tid)
  await hub.writeFile(`${basePath}/initialization`, defaultInit, fs, tid)
}

export async function removeSessionConfig({
  hub,
  configScope,
  selectedTargetId,
  type,
  name,
}) {
  const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)
  await hub.rmDir(`${prefix}${type}/${name}`, fs, tid)
}

export async function applyQuickSetup({
  hub,
  configScope,
  selectedTargetId,
  dest,
  content,
}) {
  const { fs, tid, prefix } = settingsFsContext(configScope, selectedTargetId)
  const parentDir = dest.replace(/\/[^/]+$/, '')

  await hub.mkDir(`${prefix}${parentDir}`, fs, tid)
  await hub.writeFile(`${prefix}${dest}`, content, fs, tid)
}

export async function initializeBotsterConfig({
  hub,
  configMetadata,
  configScope,
  selectedTargetId,
}) {
  await createSessionConfig({
    hub,
    configMetadata,
    configScope,
    selectedTargetId,
    type: 'agents',
    name: 'claude',
  })
}

export async function installSettingsTemplate({ hub, dest, content, scope, targetId }) {
  await hub.installTemplate(dest, content, scope, targetId)
  const name = pluginNameFromDest(dest)
  await hub.loadPlugin(name, targetId).catch(() => {})
  return name
}

export async function uninstallSettingsTemplate({ hub, dest, scope, targetId }) {
  await hub.uninstallTemplate(dest, scope, targetId)
  return pluginNameFromDest(dest)
}

export async function reloadSettingsPlugin({ hub, name, targetId }) {
  await hub.reloadPlugin(name, targetId)
}

function pluginNameFromDest(dest) {
  const match = dest?.match(/plugins\/([^/]+)\//)
  return match ? match[1] : dest
}
