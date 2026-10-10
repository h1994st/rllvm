import { describe, expect, test } from 'claude-code/testing'
import type { Engine, Plugin } from 'claude-code/testing'
import type { On } from 'claude-code'

import type { OverlayGroup, OverlayView } from '../types'

// Payloads below are what `rllvm-query mcp` answered for
// examples/callgraph-overlay, trimmed to the fields the view reads.
const MODULE = '5d2b02ee38f49552d9f8caff29186aad78f221fe812af60879a9e1a49f6e3e9b'
const HANDLER = { module_id: MODULE, symbol: 'handler' }
const DISPATCH_SITE = {
  block_index: 0,
  function: { module_id: MODULE, symbol: 'dispatch' },
  instruction_index: 5,
}

const CANDIDATES = {
  query: { kind: 'resolution_candidates' },
  results: [
    {
      candidates: [
        {
          assignments: [
            {
              field: { basis: 'struct_gep', field: { offset: 8, record: 'ops' }, name: 'on_event' },
              in_function: { module_id: MODULE, symbol: 'install' },
              in_global: null,
              kind: 'stored_to_memory',
              location: { column: 43, file: 'ops.c', line: 8 },
              used: HANDLER,
            },
          ],
          function: HANDLER,
          signature_matches: true,
        },
      ],
      field: { offset: 8, record: 'ops' },
      field_name: 'on_event',
      signature: 'void (i32)',
      single_candidate: true,
      sites: [
        {
          location: { column: 32, file: 'ops.c', line: 10, source_status: 'current' },
          site: DISPATCH_SITE,
        },
      ],
    },
  ],
  schema_version: 2,
}

// record_edges after an add through ops@8, a verify of it, and an add for the
// dispatch site itself: three unsaved records.
const SUMMARY = {
  coverage: { covered_sites: 1, unresolved_sites: 1 },
  edges: [
    {
      confidence: 'high',
      key: { to: HANDLER, via_field: { offset: 8, record: 'ops' } },
      provenance: ['ops.c:8: o->on_event = handler'],
      sites: 1,
      verification: { tool: 'reach', verdict: 'confirmed' },
    },
    {
      confidence: 'medium',
      key: { site: DISPATCH_SITE, to: HANDLER },
      provenance: ['ops.c:10'],
      sites: 1,
    },
  ],
  fingerprint: '266f51c9347b98fd0262c2d869fa762bd93a1967a23d5bbcebf64b3dcd03bf43',
  path: '/work/build/catalog.overlay.jsonl',
  pending: 3,
}

const SAVED = { path: '/work/build/catalog.overlay.jsonl', saved: 3 }

const SERVER = 'mcp__plugin_rllvm_rllvm-query__'
const PROBE = 'probe-overlay-state'
const COMMAND_RUN = {
  args: '',
  origin: { kind: 'composer' },
  presentation: { isFullscreen: true, columns: 120 },
} as const

const PANE_PROPS = {
  title: 'Call-graph overlay',
  isFocused: false,
  bodyColumns: 80,
  placement: 'dock',
  view: {},
} as const

// The tool beneath the plugin answers every call with the payload last set,
// as MCP text; `isError` makes it the errored result a failed call is.
const answering = (on: On, first: unknown) => {
  let answer = { payload: first, isError: false }
  on('tool.call', () => {
    const text =
      typeof answer.payload === 'string' ? answer.payload : JSON.stringify(answer.payload)
    return answer.isError
      ? { isError: true as const, result: text, text }
      : { result: { content: [{ type: 'text', text }] }, text }
  })
  return (payload: unknown, isError = false) => {
    answer = { payload, isError }
  }
}

// A test's own hooks may not read state, so a small plugin beside the one
// under test answers a command with the view's two atoms.
const PROBE_PLUGIN: Plugin = {
  name: 'overlay-probe',
  register(on) {
    on('command.run', { command: 'probe-overlay-state' }, async $ => ({
      text: JSON.stringify({
        groups: (await $.state.get({ plugin: 'rllvm', key: 'groups' })).value ?? null,
        overlay: (await $.state.get({ plugin: 'rllvm', key: 'overlay' })).value ?? null,
      }),
    }))
  },
}
const PROBING = { plugins: [PROBE_PLUGIN] }

const state = async ($: Engine) => {
  const { text = '' } = await $.command.run({ command: PROBE, ...COMMAND_RUN })
  return JSON.parse(text) as { groups: OverlayGroup[] | null; overlay: OverlayView | null }
}

const mountPane = ($: Engine, surface: 'terminal' | 'desktop', rows = 40) =>
  $.ui.mount({
    plugin: 'rllvm',
    surface,
    component: 'Pane',
    requestId: 'rllvm-overlay',
    props: { ...PANE_PROPS, scroll: { offset: 0, bodyRows: rows } },
    viewport: { columns: 120, rows },
  })

describe('tool results', () => {
  test('ignores_tools_that_are_not_overlay_tools', PROBING, async ($, on) => {
    answering(on, SUMMARY)
    for (const tool of [`${SERVER}reach`, 'mcp__rllvm__list_overlay_v2', 'mcp__list_overlay']) {
      const ran = await $.tool.call({ tool: tool as `mcp__${string}__${string}` })
      expect(ran.text).toBe(JSON.stringify(SUMMARY))
    }
    expect(await state($)).toEqual({ groups: null, overlay: null })
  })

  test('a_record_edges_result_updates_the_overlay_atom', PROBING, async ($, on) => {
    answering(on, SUMMARY)
    const ran = await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })
    expect(ran.text).toBe(JSON.stringify(SUMMARY))
    expect((await state($)).overlay).toEqual({
      path: '/work/build/catalog.overlay.jsonl',
      pending: 3,
      unresolved_sites: 1,
      covered_sites: 1,
      edges: [
        {
          key: { via_field: { record: 'ops', offset: 8 }, to: HANDLER },
          confidence: 'high',
          verdict: 'confirmed',
          sites: 1,
        },
        {
          key: { site: DISPATCH_SITE, to: HANDLER },
          confidence: 'medium',
          verdict: null,
          sites: 1,
        },
      ],
    })
  })

  test('a_hand_configured_server_and_a_result_without_text_still_count', PROBING, async ($, on) => {
    on('tool.call', () => ({
      result: { content: [{ type: 'text', text: JSON.stringify(SUMMARY) }] },
    }))
    await $.tool.call({ tool: 'mcp__rllvm__list_overlay' })
    expect((await state($)).overlay?.pending).toBe(3)
  })

  test('a_resolution_candidates_result_updates_the_groups_atom', PROBING, async ($, on) => {
    answering(on, CANDIDATES)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    expect((await state($)).groups).toEqual([
      {
        field: { record: 'ops', offset: 8 },
        slot: null,
        field_name: 'on_event',
        signature: 'void (i32)',
        sites: [{ site: DISPATCH_SITE, location: 'ops.c:10' }],
        candidates: [HANDLER],
        single_candidate: true,
      },
    ])
  })

  test('an_error_result_changes_nothing', PROBING, async ($, on) => {
    const answer = answering(on, SUMMARY)
    await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })
    const before = await state($)
    expect(before.overlay?.pending).toBe(3)

    const refusal =
      'Invalid arguments: record 1: no unresolved indirect call site dispatches through nope@8'
    answer(refusal, true)
    const ran = await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })
    expect(ran.isError).toBe(true)
    expect(ran.text).toBe(refusal)

    // An errored result is ignored even when its text would parse.
    answer({ ...SUMMARY, pending: 9 }, true)
    await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })

    answer('not json')
    expect((await $.tool.call({ tool: `${SERVER}list_overlay` })).text).toBe('not json')

    answer({ coverage: 'nonsense' })
    await $.tool.call({ tool: `${SERVER}load_overlay` })

    expect(await state($)).toEqual(before)
  })

  test('a_verdict_that_is_no_verdict_changes_nothing', PROBING, async ($, on) => {
    const answer = answering(on, SUMMARY)
    await $.tool.call({ tool: `${SERVER}list_overlay` })
    const before = await state($)

    // An inherited property name is not one of the verdicts.
    const odd = structuredClone(SUMMARY) as typeof SUMMARY
    odd.pending = 4
    odd.edges[0]!.verification = { tool: 'reach', verdict: 'constructor' }
    answer(odd)
    await $.tool.call({ tool: `${SERVER}list_overlay` })

    expect(await state($)).toEqual(before)
  })

  test('save_overlay_clears_the_unsaved_count', PROBING, async ($, on) => {
    const answer = answering(on, SUMMARY)
    await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })
    answer(SAVED)
    await $.tool.call({ tool: `${SERVER}save_overlay` })
    const { overlay } = await state($)
    expect(overlay?.pending).toBe(0)
    expect(overlay?.edges).toHaveLength(2)
  })
})

describe('pane', () => {
  test('the_pane_renders_coverage_and_a_field_group', async ($, on) => {
    const answer = answering(on, CANDIDATES)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    answer(SUMMARY)
    await $.tool.call({ tool: `${SERVER}record_edges`, records: [] })

    for (const surface of ['terminal', 'desktop'] as const) {
      const ui = await mountPane($, surface)
      expect((await ui.find({ key: 'coverage' }))?.text).toBe(
        'covered 1 of 1 unresolved sites · 2 edges · 3 unsaved',
      )
      expect((await ui.find({ key: 'group-ops@8' }))?.text).toMatch(/^ops@8 \(on_event\)/)
      expect((await ui.find({ key: 'candidate-ops@8-handler' }))?.text).toMatch(
        /^→ handler +high ✓$/,
      )
      expect((await ui.find({ key: 'sites' }))?.text).toBe('sites')
      expect((await ui.find({ key: 'site-0' }))?.text).toMatch(
        /^dispatch ops\.c:10 → handler +medium unverified$/,
      )
      expect(await ui.find({ text: 'Run resolution_candidates' })).toBeUndefined()
      await ui.unmount()
    }
  })

  test('a_refuted_edge_is_dim_and_an_unclaimed_candidate_is_not_marked', async ($, on) => {
    const OTHER = { module_id: MODULE, symbol: 'fallback' }
    const groups = structuredClone(CANDIDATES)
    groups.results[0]!.candidates.push({
      assignments: [],
      function: OTHER,
      signature_matches: true,
    })
    const refuted = structuredClone(SUMMARY) as typeof SUMMARY
    refuted.edges[0]!.verification = { tool: 'reach', verdict: 'refuted' }
    const answer = answering(on, groups)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    answer(refuted)
    await $.tool.call({ tool: `${SERVER}list_overlay` })

    const ui = await mountPane($, 'terminal')
    const claimed = await ui.find({ key: 'candidate-ops@8-handler' })
    expect(claimed?.text).toMatch(/^→ handler +high ✗$/)
    expect(claimed?.children[0]).toMatchObject({ type: 'Text', props: { dimColor: true } })
    const other = await ui.find({ key: 'candidate-ops@8-fallback' })
    expect(other?.text).toBe('  fallback')
  })

  test('an_empty_pane_says_how_to_fill_it', async $ => {
    for (const surface of ['terminal', 'desktop'] as const) {
      const ui = await mountPane($, surface)
      expect((await ui.find({ key: 'empty' }))?.text).toBe(
        'Run resolution_candidates to see unresolved fields.',
      )
      await ui.unmount()
    }
  })

  test('an_overlay_loaded_before_any_candidates_still_lists_its_edges', async ($, on) => {
    answering(on, SUMMARY)
    await $.tool.call({ tool: `${SERVER}load_overlay` })

    const ui = await mountPane($, 'terminal')
    expect((await ui.find({ key: 'empty' }))?.text).toBe(
      'Run resolution_candidates to see unresolved fields.',
    )
    expect((await ui.find({ key: 'group-ops@8' }))?.text).toBe('ops@8')
    expect((await ui.find({ key: 'candidate-ops@8-handler' }))?.text).toMatch(/^→ handler +high ✓$/)
    expect((await ui.find({ key: 'site-0' }))?.text).toMatch(
      /^dispatch b0:i5 → handler +medium unverified$/,
    )
  })

  test('a_group_without_a_field_is_named_by_its_site', async ($, on) => {
    const bySite = structuredClone(CANDIDATES) as { results: Record<string, unknown>[] }
    delete bySite.results[0]!.field
    delete bySite.results[0]!.field_name
    const answer = answering(on, bySite)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    answer(SUMMARY)
    await $.tool.call({ tool: `${SERVER}list_overlay` })

    const ui = await mountPane($, 'terminal')
    const id = `site-${MODULE}:dispatch:0:5`
    expect((await ui.find({ key: `group-${id}` }))?.text).toMatch(
      /^dispatch ops\.c:10 \(void \(i32\)\)/,
    )
    expect((await ui.find({ key: `candidate-${id}-handler` }))?.text).toMatch(
      /^→ handler +medium unverified$/,
    )
    // The field edge has no group of its own here, so it is listed apart.
    expect((await ui.find({ key: 'group-ops@8' }))?.text).toBe('ops@8')
  })

  test('a_slot_group_lists_the_slot_and_its_slot_edges', async ($, on) => {
    const RUN = { module_id: MODULE, symbol: 'run' }
    const slot = { kind: 'param', function: RUN, index: 0 }
    const bySlot = structuredClone(CANDIDATES) as { results: Record<string, unknown>[] }
    delete bySlot.results[0]!.field
    delete bySlot.results[0]!.field_name
    bySlot.results[0]!.slot = slot
    const summary = structuredClone(SUMMARY) as { edges: Record<string, unknown>[] }
    summary.edges = [{ ...summary.edges[0]!, key: { to: HANDLER, via_slot: slot } }]
    const answer = answering(on, bySlot)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    answer(summary)
    await $.tool.call({ tool: `${SERVER}list_overlay` })

    const ui = await mountPane($, 'terminal')
    const id = `param:${MODULE}:run:0`
    expect((await ui.find({ key: `group-${id}` }))?.text).toMatch(/^param 0 of run/)
    expect((await ui.find({ key: `candidate-${id}-handler` }))?.text).toMatch(
      /^→ handler +high ✓$/,
    )
  })

  test('a_short_pane_shows_what_fits_and_how_many_more', PROBING, async ($, on) => {
    const many = structuredClone(CANDIDATES)
    many.results = Array.from({ length: 60 }, (_, i) => ({
      ...structuredClone(CANDIDATES.results[0]!),
      field: { offset: i * 8, record: 'ops' },
    }))
    answering(on, many)
    await $.tool.call({ tool: `${SERVER}resolution_candidates` })
    const kept = (await state($)).groups ?? []
    // The server lists field groups first, so the first 50 of 60 are kept.
    expect(kept).toHaveLength(50)
    expect(kept[0]?.field).toEqual({ record: 'ops', offset: 0 })
    expect(kept[49]?.field).toEqual({ record: 'ops', offset: 392 })

    const ui = await mountPane($, 'terminal', 8)
    // Eight rows hold the coverage line, three groups of two lines each, and
    // the count of the rest: 47 groups that did not fit and 10 never kept.
    expect(await ui.find({ key: 'group-ops@0' })).toBeDefined()
    expect(await ui.find({ key: 'group-ops@16' })).toBeDefined()
    expect(await ui.find({ key: 'group-ops@24' })).toBeUndefined()
    expect((await ui.find({ key: 'more' }))?.text).toBe('…57 more')

    // With room for every kept group, the ten never kept are still counted.
    const tall = await mountPane($, 'desktop', 200)
    expect(await tall.find({ key: 'group-ops@392' })).toBeDefined()
    expect((await tall.find({ key: 'more' }))?.text).toBe('…10 more')
  })
})

describe('command', () => {
  test('overlay_view_opens_the_pane', async ($, on) => {
    const opened: unknown[] = []
    on('ui.open', ($, e) => {
      opened.push({ id: e.id, title: e.title })
      return { value: { isPlaced: true as const } }
    })
    const answer = await $.command.run({ command: 'overlay-view', ...COMMAND_RUN })
    expect(opened).toEqual([{ id: 'rllvm-overlay', title: 'Call-graph overlay' }])
    expect(answer).toMatchObject({ text: expect.stringContaining('overlay') })
  })
})
