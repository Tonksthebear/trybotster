import React, { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useSettingsStore } from '../../store/settings-store'
import { Button } from '../catalyst/button'
import {
  Dialog,
  DialogTitle,
  DialogDescription,
  DialogBody,
  DialogActions,
} from '../catalyst/dialog'
import { Input } from '../catalyst/input'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Text } from '../catalyst/text'
import SpawnTargetBrowser from '../forms/SpawnTargetBrowser'

// ─── Hub Identity Form ─────────────────────────────────────────────

function HubIdentityForm({ hubName, hubIdentifier, hubSettingsPath, hubPath }) {
  const navigate = useNavigate()
  const [name, setName] = useState(hubName || '')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState(null)

  async function handleSubmit(e) {
    e.preventDefault()
    setSaving(true)
    setError(null)

    try {
      const csrfToken = document.querySelector('meta[name="csrf-token"]')?.content
      const response = await fetch(hubSettingsPath, {
        method: 'PATCH',
        headers: {
          'Content-Type': 'application/json',
          'X-CSRF-Token': csrfToken,
          Accept: 'text/html',
        },
        body: JSON.stringify({ hub: { name } }),
        redirect: 'follow',
      })

      if (response.ok || response.redirected) {
        // Navigate back to hub to see updated name
        navigate(hubPath)
      } else {
        setError(`Save failed (${response.status})`)
        setSaving(false)
      }
    } catch (err) {
      setError(`Save failed: ${err.message}`)
      setSaving(false)
    }
  }

  return (
    <div className="border border-zinc-800 rounded-lg">
      <div className="px-4 py-3 border-b border-zinc-800">
        <h2 className="text-sm font-medium text-zinc-400">Hub Identity</h2>
      </div>
      <form onSubmit={handleSubmit} className="px-4 py-4 space-y-4">
        <Field>
          <Label className="!text-xs !text-zinc-500">Name</Label>
          <Input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={hubIdentifier}
            className="font-mono"
          />
          <Description className="!text-xs !text-zinc-600 mt-1.5">
            Identifier: <span className="font-mono">{hubIdentifier}</span>
          </Description>
        </Field>
        {error && (
          <p className="text-sm text-red-400">{error}</p>
        )}
        <div className="flex justify-end">
          <Button
            type="submit"
            color="emerald"
            disabled={saving}
          >
            {saving ? 'Saving...' : 'Save'}
          </Button>
        </div>
      </form>
    </div>
  )
}

// ─── Spawn Targets ─────────────────────────────────────────────────

function SpawnTargetsPanel({ hubId }) {
  return (
    <div className="border border-zinc-800 rounded-lg">
      <div className="px-4 py-3 border-b border-zinc-800">
        <h2 className="text-sm font-medium text-zinc-400">Spawn Targets</h2>
        <Text className="!text-xs mt-0.5">
          Directories where this hub can spawn sessions. Managed on the hub device.
        </Text>
      </div>
      <div className="px-4 py-4">
        <SpawnTargetBrowser hubId={hubId} />
      </div>
    </div>
  )
}

// ─── Hub Controls ──────────────────────────────────────────────────

function HubControls() {
  const restartHub = useSettingsStore((s) => s.restartHub)
  const [restarting, setRestarting] = useState(false)

  function handleRestart() {
    setRestarting(true)
    restartHub()
  }

  return (
    <div className="border border-zinc-800 rounded-lg">
      <div className="px-4 py-3 border-b border-zinc-800">
        <h2 className="text-sm font-medium text-zinc-400">Hub Controls</h2>
      </div>
      <div className="px-4 py-4 flex items-center justify-between gap-4">
        <div>
          <p className="text-sm text-zinc-300 font-medium">Restart Hub</p>
          <Text className="!text-xs mt-0.5">
            Gracefully restarts the hub process. Running agents are preserved
            and reconnect automatically within ~120 s.
          </Text>
        </div>
        <Button
          color="amber"
          disabled={restarting}
          onClick={handleRestart}
          className="shrink-0"
        >
          {restarting ? 'Restarting...' : 'Restart'}
        </Button>
      </div>
    </div>
  )
}

// ─── Danger Zone ───────────────────────────────────────────────────

function DangerZone({ hubSettingsPath, hubName }) {
  const navigate = useNavigate()
  const [confirmOpen, setConfirmOpen] = useState(false)
  const [deleting, setDeleting] = useState(false)

  async function handleDelete() {
    setDeleting(true)
    try {
      const csrfToken = document.querySelector('meta[name="csrf-token"]')?.content
      const response = await fetch(hubSettingsPath, {
        method: 'DELETE',
        headers: {
          'X-CSRF-Token': csrfToken,
          Accept: 'text/html',
        },
        redirect: 'follow',
      })

      if (response.ok || response.redirected) {
        navigate('/hubs')
      }
    } catch {
      setDeleting(false)
      setConfirmOpen(false)
    }
  }

  return (
    <>
      <div className="border border-red-500/30 rounded-lg">
        <div className="px-4 py-3 border-b border-red-500/20">
          <h2 className="text-sm font-medium text-red-400">Danger Zone</h2>
        </div>
        <div className="px-4 py-4 flex items-center justify-between gap-4">
          <div>
            <p className="text-sm text-zinc-300 font-medium">Delete Hub</p>
            <Text className="!text-xs mt-0.5">
              Permanently removes this hub and all associated data. This cannot
              be undone.
            </Text>
          </div>
          <Button
            color="red"
            onClick={() => setConfirmOpen(true)}
            className="shrink-0"
          >
            Delete Hub
          </Button>
        </div>
      </div>

      <Dialog open={confirmOpen} onClose={() => setConfirmOpen(false)} size="sm">
        <DialogTitle>Delete Hub</DialogTitle>
        <DialogDescription>
          Are you sure you want to delete{' '}
          <strong className="text-white">{hubName}</strong>?
        </DialogDescription>
        <DialogBody>
          <Text>
            This will remove the hub registration and all associated tokens. The
            CLI process on the device will not be affected.
          </Text>
        </DialogBody>
        <DialogActions>
          <Button plain onClick={() => setConfirmOpen(false)}>
            Cancel
          </Button>
          <Button color="red" disabled={deleting} onClick={handleDelete}>
            {deleting ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogActions>
      </Dialog>
    </>
  )
}

// ─── Main Component ────────────────────────────────────────────────

export default function HubInfoPanel({
  hubId,
  hubName,
  hubIdentifier,
  hubSettingsPath,
  hubPath,
}) {
  return (
    <div className="max-w-3xl mx-auto px-4 py-6 lg:py-8 space-y-6">
      <HubIdentityForm
        hubName={hubName}
        hubIdentifier={hubIdentifier}
        hubSettingsPath={hubSettingsPath}
        hubPath={hubPath}
      />
      <SpawnTargetsPanel hubId={hubId} />
      <HubControls />
      <DangerZone hubSettingsPath={hubSettingsPath} hubName={hubName} />
    </div>
  )
}
