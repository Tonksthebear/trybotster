import React from 'react'
import { useParams } from 'react-router-dom'
import TerminalView from '../terminal/TerminalView'

export default function SessionShow() {
  const { hubId, sessionUuid } = useParams()

  return <TerminalView hubId={hubId} sessionUuid={sessionUuid} />
}
