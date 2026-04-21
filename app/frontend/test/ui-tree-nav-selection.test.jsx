import { describe, it, expect } from 'vitest'
import {
  _applyNavSelectionOverridesForTests as applyNavSelectionOverrides,
  _navEntryMatchesPathnameForTests as navEntryMatchesPathname,
} from '../components/UiTree'

// Phase 4a F3: browser-local decorator that marks `botster.nav.open`
// tree_items as `selected` when their hub-scoped path matches
// `window.location.pathname`. Tested at the pure-function level so the
// store/router dependencies don't have to be spun up for each variant.

function navItem(path, overrides = {}) {
  return {
    type: 'tree_item',
    props: {
      action: {
        id: 'botster.nav.open',
        payload: { path },
      },
      ...overrides,
    },
  }
}

describe('navEntryMatchesPathname', () => {
  it('matches an exact hub-scoped path', () => {
    const action = { id: 'botster.nav.open', payload: { path: '/plugins/hello' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1/plugins/hello')).toBe(true)
  })

  it('tolerates a missing leading slash on the payload path', () => {
    const action = { id: 'botster.nav.open', payload: { path: 'plugins/hello' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1/plugins/hello')).toBe(true)
  })

  it('matches the root path to /hubs/<hubId>', () => {
    const action = { id: 'botster.nav.open', payload: { path: '/' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1')).toBe(true)
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1/')).toBe(true)
  })

  it('tolerates trailing-slash variants', () => {
    const action = { id: 'botster.nav.open', payload: { path: '/plugins/hello' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1/plugins/hello/')).toBe(true)
  })

  it('does not match unrelated actions', () => {
    const action = { id: 'botster.session.select', payload: { path: '/plugins/hello' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h1/plugins/hello')).toBe(false)
  })

  it('does not match when hub ids differ', () => {
    const action = { id: 'botster.nav.open', payload: { path: '/plugins/hello' } }
    expect(navEntryMatchesPathname(action, 'h1', '/hubs/h2/plugins/hello')).toBe(false)
  })

  it('returns false when hubId is empty', () => {
    const action = { id: 'botster.nav.open', payload: { path: '/plugins/hello' } }
    expect(navEntryMatchesPathname(action, '', '/hubs/h1/plugins/hello')).toBe(false)
  })
})

describe('applyNavSelectionOverrides', () => {
  it('marks only the matching tree_item as selected', () => {
    const tree = {
      type: 'tree',
      children: [
        navItem('/plugins/hello'),
        navItem('/plugins/other'),
      ],
    }

    const result = applyNavSelectionOverrides(tree, 'h1', '/hubs/h1/plugins/hello')
    expect(result.children[0].props.selected).toBe(true)
    expect(result.children[1].props.selected).toBeUndefined()
  })

  it('does not mutate the input tree', () => {
    const tree = {
      type: 'tree',
      children: [navItem('/plugins/hello')],
    }
    const snapshot = JSON.parse(JSON.stringify(tree))
    applyNavSelectionOverrides(tree, 'h1', '/hubs/h1/plugins/hello')
    expect(tree).toEqual(snapshot)
  })

  it('recurses into children and slots', () => {
    const tree = {
      type: 'tree',
      children: [
        {
          type: 'tree_item',
          props: { id: 'wrapper' },
          slots: {
            children: [navItem('/plugins/hello')],
          },
        },
      ],
    }
    const result = applyNavSelectionOverrides(tree, 'h1', '/hubs/h1/plugins/hello')
    expect(result.children[0].slots.children[0].props.selected).toBe(true)
  })

  it('does not mark selection when hubId is missing', () => {
    const tree = { type: 'tree', children: [navItem('/plugins/hello')] }
    const result = applyNavSelectionOverrides(tree, '', '/hubs/h1/plugins/hello')
    expect(result.children[0].props.selected).toBeUndefined()
  })

  it('preserves the existing tree_item.selected = false when nothing matches', () => {
    const tree = {
      type: 'tree',
      children: [{
        type: 'tree_item',
        props: {
          selected: false,  // authored-as-false; decorator must not flip to true
          action: { id: 'botster.nav.open', payload: { path: '/plugins/other' } },
        },
      }],
    }
    const result = applyNavSelectionOverrides(tree, 'h1', '/hubs/h1/plugins/hello')
    // Not a match → selected prop stays whatever was authored (false here).
    expect(result.children[0].props.selected).toBe(false)
  })
})
