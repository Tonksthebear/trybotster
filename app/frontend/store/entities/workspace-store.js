// Wire protocol — workspace entity store. Wire id field is `workspace_id`.
// The workspace entity does NOT carry a session list — the session_list
// composite derives membership client-side by filtering sessions where
// session.workspace_id == workspace.id (design brief §12.5).

import { createEntityStore } from './createEntityStore'

export const useWorkspaceEntityStore = createEntityStore('workspace', {
  idField: 'workspace_id',
})
