// Wire protocol v2 — singleton entity stores for hub metadata.
//
// Both `hub` (lifecycle / recovery state) and `connection_code` (pairing
// QR + URL) are modelled as singleton entity types whose id is the hub_id
// (design brief §3 + §12.7). Each store typically holds exactly one entity.

import { createEntityStore } from './createEntityStore'

export const useHubMetaStore = createEntityStore('hub', { idField: 'hub_id' })

export const useConnectionCodeStore = createEntityStore('connection_code', {
  idField: 'hub_id',
})
