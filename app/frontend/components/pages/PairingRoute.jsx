import React from 'react'
import { useParams } from 'react-router'
import PairingPage from '../pairing/PairingPage'

export default function PairingRoute() {
  const { hubId } = useParams()

  return (
    <PairingPage
      hubId={hubId}
      redirectUrl={`/hubs/${hubId}`}
    />
  )
}
