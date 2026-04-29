import { createEntityStore } from './createEntityStore'

// Hub-owned template catalog. Rails no longer parses catalog/templates; the
// browser receives template entities from the hub and groups them for display.
export const useTemplateStore = createEntityStore('template', { idField: 'id' })
