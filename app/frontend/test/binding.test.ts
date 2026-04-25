// Wire protocol — frontend $bind / bind_list resolver tests.
//
// Mirrors `cli/src/tui/ui_contract_adapter/binding.rs` test cases. Both
// resolvers must agree on path grammar, sentinel detection, and bind_list
// expansion — these tests + the Rust ones are the redundant evidence.

import { beforeEach, describe, expect, it } from 'vitest'

import {
  applyEntityFrame,
  _resetEntityStoresForTest,
} from '../store/entities'
import {
  countBindings,
  resolveBindings,
  resolvePath,
} from '../ui_contract/binding'

describe('resolveBindings', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'session',
      items: [
        {
          session_uuid: 'sess-a',
          title: 'alpha',
          is_idle: false,
          hosted_preview: { status: 'running', url: 'https://x' },
        },
        {
          session_uuid: 'sess-b',
          title: 'beta',
          is_idle: true,
        },
      ],
      snapshot_seq: 1,
    })
  })

  it('resolves a scalar field path', () => {
    const out = resolveBindings({ $bind: '/session/sess-a/title' })
    expect(out).toBe('alpha')
  })

  it('resolves a whole-record path', () => {
    const out = resolveBindings({ $bind: '/session/sess-a' })
    expect(out).toMatchObject({ title: 'alpha', is_idle: false })
  })

  it('resolves a list path to an array of records sorted by store order', () => {
    const out = resolveBindings({ $bind: '/session' })
    expect(Array.isArray(out)).toBe(true)
    expect(out).toHaveLength(2)
    expect((out as any[])[0].title).toBe('alpha')
    expect((out as any[])[1].title).toBe('beta')
  })

  it('returns null for unknown id / field / type', () => {
    expect(resolveBindings({ $bind: '/session/unknown/title' })).toBeNull()
    expect(resolveBindings({ $bind: '/session/sess-a/missing_field' })).toBeNull()
    expect(resolveBindings({ $bind: '/never_seen/x/title' })).toBeNull()
  })

  it('walks into nested props trees', () => {
    const out = resolveBindings({
      type: 'text',
      props: {
        text: { $bind: '/session/sess-a/title' },
        tone: 'default',
      },
    })
    expect((out as any).props.text).toBe('alpha')
    expect((out as any).props.tone).toBe('default')
  })

  it('expands bind_list into a per-item array with @-relative paths resolved', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/session',
      item_template: {
        type: 'tree_item',
        id: { $bind: '@/session_uuid' },
        slots: {
          title: [{ type: 'text', props: { text: { $bind: '@/title' } } }],
        },
      },
    })
    expect(Array.isArray(out)).toBe(true)
    expect((out as any[])[0].id).toBe('sess-a')
    expect((out as any[])[0].slots.title[0].props.text).toBe('alpha')
    expect((out as any[])[1].id).toBe('sess-b')
  })

  it('@-relative path outside bind_list resolves null', () => {
    const out = resolveBindings({ $bind: '@/title' })
    expect(out).toBeNull()
  })

  it('@-relative path with no field returns the whole item', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/session',
      item_template: { $bind: '@' },
    })
    expect((out as any[])[0].title).toBe('alpha')
  })

  it('preserves class instances unchanged (does not collapse to {})', () => {
    class Box {
      get type() {
        return 'stack'
      }
    }
    const box = new Box()
    expect(resolveBindings(box)).toBe(box)
  })

  it('an object with $bind plus another key is not treated as a sentinel', () => {
    const out = resolveBindings({
      type: 'text',
      props: { text: 'hi', $bind: '/session/sess-a/title' },
    })
    expect((out as any).props.$bind).toBe('/session/sess-a/title')
  })
})

describe('countBindings', () => {
  it('counts each $bind and bind_list once', () => {
    const tree = {
      type: 'stack',
      children: [
        { type: 'text', props: { text: { $bind: '/session/x/title' } } },
        {
          $kind: 'bind_list',
          source: '/session',
          item_template: { type: 'text', props: { text: { $bind: '@/title' } } },
        },
      ],
    }
    expect(countBindings(tree)).toBe(3)
  })
})

describe('resolvePath direct', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
  })

  it('returns null for empty paths', () => {
    expect(resolvePath('', undefined)).toBeNull()
  })

  it('returns null for too-many-segment paths', () => {
    expect(resolvePath('/session/sess-a/title/extra', undefined)).toBeNull()
  })
})
