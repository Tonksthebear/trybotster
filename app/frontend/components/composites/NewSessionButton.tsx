// Wire protocol — composite renderer for `ui.new_session_button{ action }`.
// Lifted into its own composite so the chooser UX can evolve without
// rebroadcasting trees: substituting label / icon / preset selectors all
// happen inside this component without a wire change.

import React, { type MouseEvent, type ReactElement } from 'react'

import type {
  NewSessionButtonPropsV1,
  UiActionV1,
} from '../../ui_contract/types'
import type { RenderContext } from '../../ui_contract/context'
import { Button } from '../catalyst/button'
import { IconGlyph } from '../../ui_contract/icons'

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
    <Button
      plain
      type="button"
      onClick={handleClick}
      data-action-id={action.id}
      data-testid="new-session-button"
      disabled={action.disabled === true}
    >
      <IconGlyph name="plus" />
      New session
    </Button>
  )
}
