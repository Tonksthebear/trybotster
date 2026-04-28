// Wire protocol — composite renderer for `ui.surface_nav{}`.
//
// The hub owns the route registry; this component turns registered surfaces
// into first-class workspace navigation without each plugin patching layout.

import React, { useMemo, useState, type ReactElement } from 'react'
import { Link } from 'react-router-dom'
import clsx from 'clsx'

// @ts-expect-error JS store module has no declarations yet.
import { useSessionStore } from '../../store/entities'
// @ts-expect-error JS store module has no declarations yet.
import { selectRoutesForHub, useRouteRegistryStore } from '../../store/route-registry-store'
import type { RenderContext } from '../../ui_contract/context'
import { resolveValue } from '../../ui_contract/viewport'
import type {
  SurfaceNavProps as UiSurfaceNavProps,
  UiSurfaceDensity,
  UiValue,
} from '../../ui_contract/types'
import { IconGlyph } from '../../ui_contract/icons'

type RouteNav = {
  section?: string
  order?: number
  label?: string
  icon?: string
}

type RouteRegistryEntry = {
  path?: string
  base_path?: string
  surface?: string
  label?: string
  icon?: string
  hide_from_nav?: boolean
  nav?: RouteNav | false
}

type SessionRecord = {
  owner_plugin?: string
  surface?: string
  notification?: boolean
}

export type SurfaceNavProps = UiSurfaceNavProps & {
  ctx: RenderContext
}

export function SurfaceNav({
  section,
  density,
  ctx,
}: SurfaceNavProps): ReactElement | null {
  const [collapsed, setCollapsed] = useState(false)
  const resolvedDensity =
    resolveValue<UiSurfaceDensity>(
      density as UiValue<UiSurfaceDensity> | undefined,
      ctx.viewport,
    ) ?? 'sidebar'
  const targetSection = section ?? 'workspace'

  const routeEntries = useRouteRegistryStore((state: any) =>
    selectRoutesForHub(state, ctx.hubId),
  ) as RouteRegistryEntry[]
  const sessionOrder = useSessionStore((state: any) => state.order)
  const sessionsById = useSessionStore((state: any) => state.byId)

  const notifiedSurfaces = useMemo(() => {
    const set = new Set<string>()
    for (const id of sessionOrder as string[]) {
      const session = sessionsById[id] as SessionRecord | undefined
      if (!session?.notification) continue
      const surfaceName = session.surface || session.owner_plugin
      if (surfaceName) set.add(surfaceName)
    }
    return set
  }, [sessionOrder, sessionsById])

  const entries = useMemo(() => {
    if (!ctx.hubId) return []
    return routeEntries
      .filter((entry) => {
        if (!entry || entry.hide_from_nav || entry.nav === false) return false
        if (!entry.path || entry.path === '/') return false
        const nav = entry.nav || {}
        const navSection = nav.section ?? 'workspace'
        return navSection === targetSection
      })
      .map((entry, index) => {
        const nav = entry.nav === false ? {} : (entry.nav || {})
        const surface = entry.surface || entry.path || `route:${index}`
        return {
          key: surface,
          surface,
          href: `/hubs/${ctx.hubId}${entry.base_path || entry.path}`,
          label: nav.label || entry.label || entry.surface || entry.path || 'Plugin',
          icon: nav.icon || entry.icon || 'sparkles',
          order: nav.order ?? index,
          notification: notifiedSurfaces.has(surface),
        }
      })
      .sort((a, b) => {
        if (a.order !== b.order) return a.order - b.order
        return a.label.localeCompare(b.label)
      })
  }, [ctx.hubId, notifiedSurfaces, routeEntries, targetSection])

  if (entries.length === 0) return null

  const compact = resolvedDensity === 'sidebar'
  const anyNotification = entries.some((entry) => entry.notification)

  return (
    <nav className="mt-2 flex flex-col gap-0.5 border-t border-zinc-800/80 pt-2" aria-label="Plugins">
      <button
        type="button"
        onClick={() => setCollapsed((value) => !value)}
        aria-expanded={!collapsed}
        className={clsx(
          'group flex min-w-0 items-center gap-1 px-2 py-1 text-left text-xs font-medium uppercase tracking-wider',
          anyNotification ? 'text-amber-300' : 'text-zinc-500 hover:text-zinc-300',
        )}
      >
        <IconGlyph
          name={collapsed ? 'chevron-right' : 'chevron-down'}
          className="size-3.5 shrink-0"
        />
        <span className="min-w-0 flex-1 truncate">Plugins</span>
        {anyNotification && (
          <IconGlyph name="bell-alert" className="size-3.5 shrink-0 text-amber-400" />
        )}
      </button>
      {!collapsed && (
        <ul className="flex flex-col gap-0.5">
          {entries.map((entry) => (
            <li key={entry.key}>
              <Link
                to={entry.href}
                className={clsx(
                  'group flex min-w-0 items-center gap-2 rounded-md border-l-4 px-2 text-sm',
                  compact ? 'py-1.5' : 'py-2',
                  entry.notification ? 'border-amber-400' : 'border-transparent',
                  'text-zinc-300 hover:bg-zinc-800/50 hover:text-zinc-100',
                )}
                data-testid="surface-nav-entry"
                data-surface={entry.surface}
                data-notification={entry.notification || undefined}
              >
                <span className="inline-flex size-4 shrink-0 items-center justify-center text-zinc-500 group-hover:text-zinc-300">
                  <IconGlyph name={entry.icon} className="size-4" />
                </span>
                <span className="min-w-0 truncate text-xs font-medium">
                  {entry.label}
                </span>
                {entry.notification && (
                  <IconGlyph name="bell-alert" className="ml-auto size-3.5 shrink-0 text-amber-400" />
                )}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </nav>
  )
}
