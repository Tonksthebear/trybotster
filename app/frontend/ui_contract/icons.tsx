import React, { type ReactElement } from 'react'

/**
 * Heroicons renderer for the `icon` primitive.
 *
 * Botster vendors Heroicons under `app/assets/svg/icons/heroicons`. The web
 * runtime eagerly loads the mini set so plugin authors can reference any
 * vendored Heroicons mini filename without a frontend code change:
 *
 *   icon = "book-open"
 *
 * A few pre-existing semantic aliases are kept for older surfaces.
 */

type IconPathSet = string[]

type ImportMetaWithGlob = ImportMeta & {
  glob: (
    pattern: string,
    options: { eager: true; query: string; import: 'default' },
  ) => Record<string, string>
}

const HEROICON_SVGS = (import.meta as ImportMetaWithGlob).glob(
  '../../assets/svg/icons/heroicons/mini/*.svg',
  {
    eager: true,
    query: '?raw',
    import: 'default',
  },
)

const ICON_ALIASES: Record<string, string> = {
  close: 'x-mark',
  'external-link': 'arrow-top-right-on-square',
  globe: 'globe-alt',
  sparkle: 'sparkles',
  workspace: 'folder',
}

const ICON_PATHS: Record<string, IconPathSet> = Object.fromEntries(
  Object.entries(HEROICON_SVGS).flatMap(([path, svg]) => {
    const match = path.match(/\/([^/]+)\.svg$/)
    if (!match) return []
    const paths = Array.from(svg.matchAll(/<path[^>]*\sd="([^"]+)"/g))
      .map(([, d]) => d)
      .filter((d): d is string => typeof d === 'string' && d.length > 0)
    if (paths.length === 0) return []
    return [[match[1], paths]]
  }),
)

function resolveIconName(name: string): string {
  return ICON_ALIASES[name] || name
}

type IconGlyphProps = {
  name: string
  className?: string
}

/**
 * Render the SVG glyph for an icon name. Returns `null` when the name is not
 * in the vendored Heroicons mini set so callers can decide whether to render a
 * placeholder.
 */
export function IconGlyph({
  name,
  className,
}: IconGlyphProps): ReactElement | null {
  const paths = ICON_PATHS[resolveIconName(name)]
  if (paths === undefined) return null
  return (
    <svg
      viewBox="0 0 20 20"
      fill="currentColor"
      aria-hidden="true"
      data-slot="icon"
      className={className}
    >
      {paths.map((path, index) => (
        <path key={index} d={path} />
      ))}
    </svg>
  )
}

/** Returns true if the vendored Heroicons mini set knows the given name. */
export function hasIcon(name: string): boolean {
  return Object.hasOwn(ICON_PATHS, resolveIconName(name))
}
