// Wire protocol v2 — worktree entity store. Wire id field is `worktree_path`.

import { createEntityStore } from './createEntityStore'

export const useWorktreeStore = createEntityStore('worktree', {
  idField: 'worktree_path',
})
