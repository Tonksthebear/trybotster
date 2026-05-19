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
  bindingEntityRequests,
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
          output_activity: 'active',
          plugin_state: { example_provider: { status: 'running', url: 'https://x' } },
        },
        {
          session_uuid: 'sess-b',
          title: 'beta',
          output_activity: 'idle',
        },
      ],
      snapshot_seq: 1,
    })
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'project-pipelines.ticket',
      items: [
        { id: 'ticket-1', title: 'Add bindings', status: 'active' },
        { id: 'ticket-2', title: 'Review bindings', status: 'open' },
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
    expect(out).toMatchObject({ title: 'alpha', output_activity: 'active' })
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

  it('resolves plugin namespaced entity paths', () => {
    expect(
      resolveBindings({ $bind: '/project-pipelines.ticket/ticket-1/title' }),
    ).toBe('Add bindings')
    expect(resolveBindings({ $bind: '/project-pipelines.ticket/ticket-2' }))
      .toMatchObject({ status: 'open' })
    expect(resolveBindings({ $bind: '/project-pipelines.ticket' })).toHaveLength(2)
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

  it('flattens bind_list expansion inside children arrays', () => {
    const out = resolveBindings({
      type: 'stack',
      children: [
        {
          $kind: 'bind_list',
          source: '/project-pipelines.ticket',
          item_template: {
            type: 'text',
            props: { text: { $bind: '@/title' } },
          },
        },
      ],
    })
    expect((out as any).children).toHaveLength(2)
    expect((out as any).children[0].props.text).toBe('Add bindings')
    expect((out as any).children[1].props.text).toBe('Review bindings')
  })

  it('filters bind_list records with exact where matches', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/project-pipelines.ticket',
      where: { status: 'open' },
      item_template: {
        type: 'text',
        props: { text: { $bind: '@/title' } },
      },
    })
    expect((out as any[]).map((node) => node.props.text)).toEqual(['Review bindings'])
  })

  it('uses bind_list empty_template when no records match', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/project-pipelines.ticket',
      where: { status: 'missing' },
      item_template: {
        type: 'text',
        props: { text: { $bind: '@/title' } },
      },
      empty_template: {
        type: 'empty_state',
        props: {
          title: 'No tickets',
          description: { $bind: '/project-pipelines.ticket' },
        },
      },
    })
    expect((out as any[])).toHaveLength(1)
    expect((out as any[])[0].type).toBe('empty_state')
    expect((out as any[])[0].props.title).toBe('No tickets')
    expect((out as any[])[0].props.description).toHaveLength(2)
  })

  it('uses bind_list empty_template when the source store is missing', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/project-pipelines.pipeline',
      item_template: {
        type: 'text',
        props: { text: { $bind: '@/name' } },
      },
      empty_template: {
        type: 'empty_state',
        props: {
          title: 'No pipelines',
          description: { $bind: '@/name' },
        },
      },
    })
    expect((out as any[])).toHaveLength(1)
    expect((out as any[])[0].type).toBe('empty_state')
    expect((out as any[])[0].props.title).toBe('No pipelines')
    expect((out as any[])[0].props.description).toBeNull()
  })

  it('does not use bind_list empty_template when records match', () => {
    const out = resolveBindings({
      $kind: 'bind_list',
      source: '/project-pipelines.ticket',
      where: { status: 'open' },
      item_template: {
        type: 'text',
        props: { text: { $bind: '@/title' } },
      },
      empty_template: {
        type: 'empty_state',
        props: { title: 'No tickets' },
      },
    })
    expect((out as any[])).toHaveLength(1)
    expect((out as any[])[0].type).toBe('text')
    expect((out as any[])[0].props.text).toBe('Review bindings')
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

describe('bindingEntityRequests', () => {
  it('extracts targeted requests from detail binds and filtered lists', () => {
    const tree = {
      type: 'stack',
      children: [
        { type: 'text', props: { text: { $bind: '/project-pipelines.run/run-1/status' } } },
        {
          $kind: 'bind_list',
          source: '/project-pipelines.run_step',
          where: { run_id: 'run-1' },
          item_template: { type: 'text', props: { text: { $bind: '@/name' } } },
        },
      ],
    }

    expect(bindingEntityRequests(tree)).toEqual([
      { entity_type: 'project-pipelines.run_step', where: { run_id: 'run-1' } },
      { entity_type: 'project-pipelines.run', id: 'run-1' },
    ])
  })

  it('extracts targeted requests from bind_if paths', () => {
    const tree = {
      type: 'stack',
      children: [
        {
          $kind: 'bind_if',
          path: '/project-pipelines.ticket/ticket-1/ready',
          node: { type: 'text', props: { text: 'ready' } },
        },
      ],
    }

    expect(bindingEntityRequests(tree)).toEqual([
      { entity_type: 'project-pipelines.ticket', id: 'ticket-1' },
    ])
  })

  it('deduplicates filtered list requests with stable where-key ordering', () => {
    const tree = {
      type: 'stack',
      children: [
        {
          $kind: 'bind_list',
          source: '/project-pipelines.run_step',
          where: { run_id: 'run-1', status: 'blocked' },
          item_template: { type: 'text', props: { text: { $bind: '@/name' } } },
        },
        {
          $kind: 'bind_list',
          source: '/project-pipelines.run_step',
          where: { status: 'blocked', run_id: 'run-1' },
          item_template: { type: 'text', props: { text: { $bind: '@/name' } } },
        },
      ],
    }

    expect(bindingEntityRequests(tree)).toEqual([
      {
        entity_type: 'project-pipelines.run_step',
        where: { run_id: 'run-1', status: 'blocked' },
      },
    ])
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
