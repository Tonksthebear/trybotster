import React, { useEffect, useState } from 'react'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Input } from '../catalyst/input'
import { Select } from '../catalyst/select'

// Controlled workspace picker: "None" | existing workspace | new workspace name.
// value shape: { id: string|null, name: string|null }
//   id    → existing workspace id (name is the display label from the hub)
//   name  → only set when id is null and user is creating a new workspace
//   both null → no workspace
export default function WorkspacePicker({ workspaces, value, onChange, description }) {
  const existing = (workspaces || []).filter(
    (ws) => ws && typeof ws === 'object' && ws.id
  )

  // Picker mode: 'none' | 'existing' | 'new'
  const initialMode = value?.id ? 'existing' : value?.name ? 'new' : 'none'
  const [mode, setMode] = useState(initialMode)
  const [newName, setNewName] = useState(value?.name || '')

  // Reset picker state when `value` is cleared externally (e.g. dialog close).
  useEffect(() => {
    if (!value?.id && !value?.name) {
      setMode('none')
      setNewName('')
    }
  }, [value?.id, value?.name])

  function handleModeChange(e) {
    const next = e.target.value
    setMode(next)
    if (next === 'none') {
      onChange(null)
    } else if (next === 'new') {
      onChange(newName.trim() ? { id: null, name: newName.trim() } : null)
    } else {
      // Selecting 'existing' without a pick yet means "no selection"
      onChange(null)
    }
  }

  function handleExistingChange(e) {
    const id = e.target.value
    if (!id) {
      onChange(null)
      return
    }
    const ws = existing.find((w) => w.id === id)
    onChange({ id, name: ws?.name || null })
  }

  function handleNewNameChange(e) {
    const name = e.target.value
    setNewName(name)
    onChange(name.trim() ? { id: null, name: name.trim() } : null)
  }

  return (
    <Field>
      <Label>Workspace</Label>
      <Select value={mode} onChange={handleModeChange}>
        <option value="none">Default</option>
        {existing.length > 0 && <option value="existing">Use existing workspace</option>}
        <option value="new">Create new workspace</option>
      </Select>

      {mode === 'existing' && existing.length > 0 && (
        <Select
          value={value?.id || ''}
          onChange={handleExistingChange}
          className="mt-2"
        >
          <option value="">Select a workspace</option>
          {existing.map((ws) => (
            <option key={ws.id} value={ws.id}>
              {ws.name || ws.id}
            </option>
          ))}
        </Select>
      )}

      {mode === 'new' && (
        <Input
          value={newName}
          onChange={handleNewNameChange}
          placeholder="Workspace name"
          className="mt-2"
          autoComplete="off"
          spellCheck={false}
        />
      )}

      {description && <Description>{description}</Description>}
    </Field>
  )
}
