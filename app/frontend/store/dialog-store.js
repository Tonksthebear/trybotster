import { create } from 'zustand'

export const useDialogStore = create((set) => ({
  activeDialog: null,
  context: {},

  openRename({ workspaceId, title }) {
    set({ activeDialog: 'rename', context: { workspaceId, title } })
  },

  openMove({ sessionId, sessionUuid }) {
    set({ activeDialog: 'move', context: { sessionId, sessionUuid } })
  },

  openDelete({ sessionId, sessionUuid }) {
    set({ activeDialog: 'delete', context: { sessionId, sessionUuid } })
  },

  openNewSession() {
    set({ activeDialog: 'newSession', context: {} })
  },

  openNewAgent({ targetId } = {}) {
    set({ activeDialog: 'newAgent', context: { targetId: targetId || null } })
  },

  openNewAccessory({ targetId } = {}) {
    set({ activeDialog: 'newAccessory', context: { targetId: targetId || null } })
  },

  close() {
    set({ activeDialog: null, context: {} })
  },
}))
