// Wire protocol v2 — spawn target entity store. Wire id field is `target_id`.

import { createEntityStore } from './createEntityStore'

export const useSpawnTargetStore = createEntityStore('spawn_target', {
  idField: 'target_id',
})
