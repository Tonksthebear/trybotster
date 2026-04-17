import React, { useState } from 'react'
import { Button } from './catalyst/button'
import { useWorkspaceStore } from '../store/workspace-store'

export default function ProofOfLife() {
  const [clicked, setClicked] = useState(false)
  const surface = useWorkspaceStore((s) => s.surface)
  const sessionCount = useWorkspaceStore((s) => s.sessionOrder.length)

  return (
    <div className="space-y-6 p-8 max-w-lg mx-auto">
      <h2 className="text-xl font-semibold text-white">
        Vite + React + Catalyst
      </h2>

      <p className="text-sm text-zinc-400">
        Surface: <code className="text-primary-400">{surface}</code> |
        Sessions in store: <code className="text-primary-400">{sessionCount}</code>
      </p>

      <div className="flex gap-3">
        <Button color="indigo" onClick={() => setClicked(true)}>
          {clicked ? 'Catalyst works!' : 'Click me'}
        </Button>
        <Button outline onClick={() => setClicked(false)}>
          Reset
        </Button>
      </div>

      {clicked && (
        <p className="text-sm text-success-400">
          React state, Catalyst components, zustand store, and Tailwind v4 are all working.
        </p>
      )}
    </div>
  )
}
