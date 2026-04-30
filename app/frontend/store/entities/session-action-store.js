import { createEntityStore } from './createEntityStore'

export const useSessionActionStore = createEntityStore('session_action', {
  idField: 'id',
})
