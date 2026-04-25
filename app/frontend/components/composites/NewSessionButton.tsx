// Wire protocol — composite renderer for `ui.new_session_button{ action }`.
// Lifted into its own composite so the chooser UX can evolve without
// rebroadcasting trees: substituting label / icon / preset selectors all
// happen inside this component without a wire change.

import React, { type MouseEvent, type ReactElement } from 'react'
import clsx from 'clsx'

import type {
  NewSessionButtonPropsV1,
  UiActionV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'

export type NewSessionButtonProps = NewSessionButtonPropsV1 & {
  ctx: RenderContext
}

export function NewSessionButton({
  action,
  ctx,
}: NewSessionButtonProps): ReactElement {
  const handleClick = (event: MouseEvent) => {
    event.preventDefault()
    event.stopPropagation()
    if (action.disabled) return
    ctx.dispatch(action as UiActionV1, {
      element: event.currentTarget as Element,
    })
  }
  return (
    <button
      type="button"
      onClick={handleClick}
      data-action-id={action.id}
      data-testid="new-session-button"
      disabled={action.disabled === true}
      className={clsx(
        'inline-flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium',
        'text-zinc-200 hover:bg-zinc-800/60 disabled:cursor-not-allowed disabled:opacity-50',
      )}
    >
      <span aria-hidden="true">+</span>
      New session
    </button>
  )
}
