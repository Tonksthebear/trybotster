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

export function groupedTemplatesByCategory(templateEntities) {
  const grouped = {}
  for (const template of templateEntities || []) {
    if (!template?.category) continue
    grouped[template.category] = grouped[template.category] || []
    grouped[template.category].push(template)
  }

  for (const category of Object.keys(grouped)) {
    grouped[category].sort((a, b) => (a.dest || '').localeCompare(b.dest || ''))
  }

  return grouped
}

export function agentQuickSetupTemplates(templates) {
  return (templates?.agents || []).filter((template) =>
    template.dest?.endsWith('/initialization')
  )
}

function sortedKeys(value) {
  return Object.keys(value || {}).sort()
}
