import * as Headless from '@headlessui/react'
import React, { forwardRef } from 'react'
import { Link as RouterLink } from 'react-router-dom'

function isInternalHref(href) {
  if (!href || typeof href !== 'string') return false
  if (href.startsWith('/') && !href.startsWith('//')) return true
  return false
}

export const Link = forwardRef(function Link({ href, ...props }, ref) {
  if (isInternalHref(href)) {
    return (
      <Headless.DataInteractive>
        <RouterLink to={href} {...props} ref={ref} />
      </Headless.DataInteractive>
    )
  }

  return (
    <Headless.DataInteractive>
      <a href={href} {...props} ref={ref} />
    </Headless.DataInteractive>
  )
})
