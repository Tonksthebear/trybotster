export function configTreeSections(tree) {
  if (!tree) {
    return {
      agentNames: [],
      accessoryNames: [],
      workspaceNames: [],
      pluginNames: [],
    }
  }

  return {
    agentNames: sortedKeys(tree.agents),
    accessoryNames: sortedKeys(tree.accessories),
    workspaceNames: sortedKeys(tree.workspaces),
    pluginNames: sortedKeys(tree.plugins),
  }
}

export function templateCategories(templates) {
  return Object.keys(templates || {}).sort()
}

export function flattenedTemplates(templates) {
  return Object.values(templates || {}).flat()
}

function sortedKeys(value) {
  return Object.keys(value || {}).sort()
}
