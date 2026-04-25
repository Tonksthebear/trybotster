// Wire protocol — session entity store. Wire id field is `session_uuid`.
// Covers Agent + Accessory subclasses; the `session_type` field discriminates.

import { createEntityStore } from './createEntityStore'

export const useSessionStore = createEntityStore('session', { idField: 'session_uuid' })
