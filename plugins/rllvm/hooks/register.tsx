// A read-only view of rllvm-query's call-graph overlay. It draws only from
// the results of the server's own tool calls, so records the agent has not
// saved yet show as they are made; it never reads the overlay file, never
// writes anything, and offers nothing to press.

import { atom, read, update } from 'claude-code'
import type { Register, StateDollar } from 'claude-code'

import type {
  OverlayConfidence,
  OverlayEdge,
  OverlayEdgeKey,
  OverlayField,
  OverlayFunction,
  OverlayGroup,
  OverlaySite,
  OverlaySiteAt,
  OverlaySlot,
  OverlayVerdict,
  OverlayView,
} from '../types'

const PANE = 'rllvm-overlay'
const TITLE = 'Call-graph overlay'
const COMMAND = 'overlay-view'
const EMPTY = 'Run resolution_candidates to see unresolved fields.'
const GROUP_LIMIT = 50

const CANDIDATES_TOOL = 'resolution_candidates'
const SAVE_TOOL = 'save_overlay'
const SUMMARY_TOOLS = ['record_edges', 'list_overlay', 'load_overlay']
const OVERLAY_TOOLS = [CANDIDATES_TOOL, SAVE_TOOL, ...SUMMARY_TOOLS]

const CONFIDENCES: readonly OverlayConfidence[] = ['low', 'medium', 'high']
const VERDICT_MARKS: Readonly<Record<OverlayVerdict, string>> = {
  confirmed: '✓',
  refuted: '✗',
  inconclusive: '?',
}

const groups = atom({ plugin: 'rllvm', key: 'groups' } as const, [])
const droppedGroups = atom({ plugin: 'rllvm', key: 'dropped_groups' } as const, 0)
const overlay = atom({ plugin: 'rllvm', key: 'overlay' } as const, null)

type Json = Record<string, unknown>

const isObject = (value: unknown): value is Json =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

/**
 * The rllvm-query tool a call went to, or undefined for any other tool. The
 * server is named by whoever configured it (`plugin_rllvm_rllvm-query` from
 * the rllvm plugin, anything by hand), so only the `mcp__` prefix and the
 * `__<tool>` suffix are fixed.
 */
const overlayTool = (name: string): string | undefined =>
  OVERLAY_TOOLS.find(tool => {
    const suffix = `__${tool}`
    return name.startsWith('mcp__') && name.endsWith(suffix) && name.length > 5 + suffix.length
  })

/** The JSON text an MCP result carries, whichever shape the engine handed it in. */
const resultText = (ran: { text?: string; result?: unknown }): string | undefined => {
  if (typeof ran.text === 'string') return ran.text
  const result = ran.result
  if (typeof result === 'string') return result
  const blocks = Array.isArray(result) ? result : isObject(result) ? result.content : undefined
  const first: unknown = Array.isArray(blocks) ? blocks[0] : undefined
  return isObject(first) && typeof first.text === 'string' ? first.text : undefined
}

// Readers of the server's payloads. Each answers undefined for a value that
// is not the shape the server sends, and a payload with any such value is
// ignored whole, so the view never shows half of an answer.

const allOf = <T,>(values: unknown, parse: (value: unknown) => T | undefined): T[] | undefined => {
  if (!Array.isArray(values)) return undefined
  const parsed = values.map(parse)
  return parsed.every(value => value !== undefined) ? (parsed as T[]) : undefined
}

const parseFunction = (value: unknown): OverlayFunction | undefined =>
  isObject(value) && typeof value.module_id === 'string' && typeof value.symbol === 'string'
    ? { module_id: value.module_id, symbol: value.symbol }
    : undefined

const parseField = (value: unknown): OverlayField | undefined =>
  isObject(value) && typeof value.record === 'string' && typeof value.offset === 'number'
    ? { record: value.record, offset: value.offset }
    : undefined

const parseSlot = (value: unknown): OverlaySlot | undefined => {
  if (!isObject(value)) return undefined
  if (value.kind === 'global' && typeof value.name === 'string') {
    const module_id = value.module_id ?? null
    return module_id === null || typeof module_id === 'string'
      ? { kind: 'global', name: value.name, module_id }
      : undefined
  }
  const fn = parseFunction(value.function)
  if (!fn) return undefined
  if (value.kind === 'param' && typeof value.index === 'number')
    return { kind: 'param', function: fn, index: value.index }
  return value.kind === 'returned' ? { kind: 'returned', function: fn } : undefined
}

const parseSite = (value: unknown): OverlaySite | undefined => {
  if (!isObject(value)) return undefined
  const fn = parseFunction(value.function)
  const { block_index, instruction_index } = value
  return fn && typeof block_index === 'number' && typeof instruction_index === 'number'
    ? { function: fn, block_index, instruction_index }
    : undefined
}

const parseLocation = (value: unknown): string | null =>
  isObject(value) && typeof value.file === 'string' && typeof value.line === 'number'
    ? `${value.file}:${value.line}`
    : null

const parseSiteAt = (value: unknown): OverlaySiteAt | undefined => {
  const site = isObject(value) ? parseSite(value.site) : undefined
  return site && isObject(value) ? { site, location: parseLocation(value.location) } : undefined
}

const parseGroup = (value: unknown): OverlayGroup | undefined => {
  if (!isObject(value) || typeof value.signature !== 'string') return undefined
  const field = value.field === undefined || value.field === null ? null : parseField(value.field)
  const slot = value.slot === undefined || value.slot === null ? null : parseSlot(value.slot)
  const sites = allOf(value.sites, parseSiteAt)
  const candidates = allOf(value.candidates, one =>
    isObject(one) ? parseFunction(one.function) : undefined,
  )
  if (field === undefined || slot === undefined || !sites || !candidates) return undefined
  return {
    field,
    slot,
    field_name: typeof value.field_name === 'string' ? value.field_name : null,
    signature: value.signature,
    sites,
    candidates,
    single_candidate: value.single_candidate === true,
  }
}

const parseKey = (value: unknown): OverlayEdgeKey | undefined => {
  if (!isObject(value)) return undefined
  const to = parseFunction(value.to)
  if (!to) return undefined
  if ('via_field' in value) {
    const via_field = parseField(value.via_field)
    return via_field && { via_field, to }
  }
  if ('via_slot' in value) {
    const via_slot = parseSlot(value.via_slot)
    return via_slot && { via_slot, to }
  }
  const site = parseSite(value.site)
  return site && { site, to }
}

const parseEdge = (value: unknown): OverlayEdge | undefined => {
  if (!isObject(value)) return undefined
  const key = parseKey(value.key)
  const confidence = CONFIDENCES.find(one => one === value.confidence)
  const verdict = isObject(value.verification) ? value.verification.verdict : null
  const isVerdict =
    verdict === null || (typeof verdict === 'string' && Object.hasOwn(VERDICT_MARKS, verdict))
  if (!key || !confidence || !isVerdict || typeof value.sites !== 'number') return undefined
  return { key, confidence, verdict: verdict as OverlayVerdict | null, sites: value.sites }
}

const parseSummary = (value: unknown): OverlayView | undefined => {
  if (!isObject(value) || !isObject(value.coverage)) return undefined
  const { unresolved_sites, covered_sites } = value.coverage
  const edges = allOf(value.edges, parseEdge)
  const path =
    value.path === undefined || typeof value.path === 'string' ? (value.path ?? null) : undefined
  if (
    !edges ||
    path === undefined ||
    typeof value.pending !== 'number' ||
    typeof unresolved_sites !== 'number' ||
    typeof covered_sites !== 'number'
  ) {
    return undefined
  }
  return { path, pending: value.pending, edges, unresolved_sites, covered_sites }
}

/** Keeps what a tool's answer says about the overlay; ignores anything else. */
const remember = async ($: StateDollar, tool: string, payload: unknown) => {
  if (tool === CANDIDATES_TOOL) {
    const parsed = isObject(payload) ? allOf(payload.results, parseGroup) : undefined
    if (!parsed) return
    // The server lists field and slot groups first, the ones an edge can
    // cover most sites through, so the first are kept and the rest only
    // counted.
    await update($, groups, () => parsed.slice(0, GROUP_LIMIT))
    await update($, droppedGroups, () => Math.max(0, parsed.length - GROUP_LIMIT))
  } else if (tool === SAVE_TOOL) {
    if (!isObject(payload) || typeof payload.saved !== 'number' || typeof payload.path !== 'string')
      return
    const { saved, path } = payload
    await update(
      $,
      overlay,
      view => view && { ...view, path, pending: Math.max(0, view.pending - saved) },
    )
  } else {
    const parsed = parseSummary(payload)
    if (parsed) await update($, overlay, () => parsed)
  }
}

// Labels.

const fieldLabel = (field: OverlayField) => `${field.record}@${field.offset}`

const slotLabel = (slot: OverlaySlot) =>
  slot.kind === 'global'
    ? `global ${slot.name}`
    : slot.kind === 'param'
      ? `param ${slot.index} of ${slot.function.symbol}`
      : `*${slot.function.symbol}()`

/** Identifies a slot exactly, where its label may repeat across modules. */
const slotId = (slot: OverlaySlot) =>
  slot.kind === 'global'
    ? `global:${slot.module_id ?? ''}:${slot.name}`
    : slot.kind === 'param'
      ? `param:${slot.function.module_id}:${slot.function.symbol}:${slot.index}`
      : `returned:${slot.function.module_id}:${slot.function.symbol}`

/** The field or slot a group dispatches through: its id and label, or null for neither. */
const groupPattern = (group: OverlayGroup) =>
  group.field
    ? { id: fieldLabel(group.field), label: fieldLabel(group.field) }
    : group.slot
      ? { id: slotId(group.slot), label: slotLabel(group.slot) }
      : null

/** The field or slot an edge covers, or null for a site edge. */
const edgePattern = (key: OverlayEdgeKey) =>
  'via_field' in key
    ? { id: fieldLabel(key.via_field), label: fieldLabel(key.via_field) }
    : 'via_slot' in key
      ? { id: slotId(key.via_slot), label: slotLabel(key.via_slot) }
      : null

const siteId = (site: OverlaySite) =>
  `${site.function.module_id}:${site.function.symbol}:${site.block_index}:${site.instruction_index}`

const sameFunction = (a: OverlayFunction, b: OverlayFunction) =>
  a.module_id === b.module_id && a.symbol === b.symbol

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? '' : 's'}`

const edgeMark = (edge: OverlayEdge) =>
  `${edge.confidence} ${edge.verdict ? VERDICT_MARKS[edge.verdict] : 'unverified'}`

/** A function's symbol, with its module when another listed function shares the symbol. */
const functionLabel = (fn: OverlayFunction, among: readonly OverlayFunction[]) =>
  among.some(other => other.symbol === fn.symbol && other.module_id !== fn.module_id)
    ? `${fn.symbol} (${fn.module_id.slice(0, 8)})`
    : fn.symbol

type Line = { key: string; text: string; detail?: string; isDim?: boolean; isBold?: boolean }

/** One group's lines: its header, then each candidate, marked when an edge claims it. */
const groupLines = (group: OverlayGroup, edges: readonly OverlayEdge[]): Line[] => {
  // A group with no field is named by its first site, where the call is.
  const first = group.sites[0]
  const where = first ? [first.site.function.symbol, first.location ?? ''].join(' ').trim() : ''
  const pattern = groupPattern(group)
  const id = pattern ? pattern.id : `site-${first ? siteId(first.site) : group.signature}`
  const head = pattern
    ? `${pattern.label}${group.field_name ? ` (${group.field_name})` : ''}`
    : `${where || 'sites'} (${group.signature})`
  const details = [plural(group.sites.length, 'site')]
  if (pattern) details.push(group.signature)
  if (group.single_candidate) details.push('single candidate')
  const sites = new Set(group.sites.map(one => siteId(one.site)))
  const claims = edges.filter(edge => {
    const covers = edgePattern(edge.key)
    if (covers) return pattern !== null && covers.id === pattern.id
    return pattern === null && 'site' in edge.key && sites.has(siteId(edge.key.site))
  })
  // An edge to a function the candidates do not list still belongs here.
  const targets = [
    ...group.candidates,
    ...claims
      .map(edge => edge.key.to)
      .filter(to => !group.candidates.some(one => sameFunction(one, to))),
  ]
  return [
    { key: `group-${id}`, text: head, detail: ` · ${details.join(' · ')}`, isBold: true },
    ...targets.map(target => {
      const label = functionLabel(target, targets)
      const edge = claims.find(one => sameFunction(one.key.to, target))
      return edge
        ? {
            key: `candidate-${id}-${label}`,
            text: `→ ${label}  ${edgeMark(edge)}`,
            isDim: edge.verdict === 'refuted',
          }
        : { key: `candidate-${id}-${label}`, text: `  ${label}` }
    }),
  ]
}

/** Field and slot edges whose pattern no group lists, one block per pattern. */
const orphanPatternBlocks = (
  patternEdges: readonly OverlayEdge[],
  known: ReadonlySet<string>,
): Line[][] => {
  const byPattern = new Map<string, { label: string; edges: OverlayEdge[] }>()
  for (const edge of patternEdges) {
    const pattern = edgePattern(edge.key)
    if (!pattern || known.has(pattern.id)) continue
    const entry = byPattern.get(pattern.id) ?? { label: pattern.label, edges: [] }
    byPattern.set(pattern.id, { ...entry, edges: [...entry.edges, edge] })
  }
  return [...byPattern].map(([id, { label, edges }]) => [
    { key: `group-${id}`, text: label, isBold: true },
    ...edges.map(edge => {
      const name = functionLabel(
        edge.key.to,
        edges.map(one => one.key.to),
      )
      return {
        key: `candidate-${id}-${name}`,
        text: `→ ${name}  ${edgeMark(edge)}`,
        isDim: edge.verdict === 'refuted',
      }
    }),
  ])
}

/** Edges recorded for one call site, with the site's source line when a group named it. */
const siteBlock = (
  edges: readonly OverlayEdge[],
  at: ReadonlyMap<string, string | null>,
): Line[] => {
  const lines = edges.flatMap(edge => {
    if (!('site' in edge.key)) return []
    const { site, to } = edge.key
    const where = at.get(siteId(site)) ?? `b${site.block_index}:i${site.instruction_index}`
    const text = `${site.function.symbol} ${where} → ${functionLabel(to, [])}  ${edgeMark(edge)}`
    return [{ text, isDim: edge.verdict === 'refuted' }]
  })
  return lines.length
    ? [
        { key: 'sites', text: 'sites', isBold: true },
        ...lines.map((line, index) => ({ key: `site-${index}`, ...line })),
      ]
    : []
}

const coverageLine = (view: OverlayView | null): Line =>
  view
    ? {
        key: 'coverage',
        text: [
          `covered ${view.covered_sites} of ${view.unresolved_sites} unresolved sites`,
          plural(view.edges.length, 'edge'),
          `${view.pending} unsaved`,
        ].join(' · '),
      }
    : { key: 'coverage', text: 'no overlay loaded yet', isDim: true }

/**
 * Everything the pane shows, cut to `rows` lines with a count of the blocks
 * left out, `dropped` groups never kept among them.
 */
const paneLines = (
  list: readonly OverlayGroup[],
  dropped: number,
  view: OverlayView | null,
  rows: number,
): Line[] => {
  const edges = view?.edges ?? []
  const head = [coverageLine(view)]
  if (view?.path) head.push({ key: 'path', text: view.path, isDim: true })
  if (list.length === 0) head.push({ key: 'empty', text: EMPTY, isDim: true })

  const known = new Set(list.flatMap(group => groupPattern(group)?.id ?? []))
  const at = new Map(
    list.flatMap(group => group.sites.map(one => [siteId(one.site), one.location] as const)),
  )
  const blocks = [
    ...list.map(group => groupLines(group, edges)),
    ...orphanPatternBlocks(edges, known),
    siteBlock(edges, at),
  ].filter(block => block.length > 0)

  const more = (count: number): Line => ({ key: 'more', text: `…${count} more`, isDim: true })
  const shown = [...head]
  for (const [index, block] of blocks.entries()) {
    const isLast = index === blocks.length - 1 && dropped === 0
    if (shown.length + block.length + (isLast ? 0 : 1) > rows) {
      shown.push(more(blocks.length - index + dropped))
      return shown
    }
    shown.push(...block)
  }
  if (dropped > 0) shown.push(more(dropped))
  return shown
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: COMMAND,
      description: "Show rllvm-query's call-graph overlay in a pane",
    })
    return next(e)
  })

  on('command.run', { command: COMMAND }, async $ => {
    await $.ui.open({ id: PANE, title: TITLE })
    return { text: 'Call-graph overlay pane opened.' }
  })

  on('tool.call', async ($, e, next) => {
    const tool = overlayTool(String(e.tool))
    if (!tool) return next(e)
    const ran = await next(e)
    if (ran.deny !== undefined || ran.isError === true) return ran
    const text = resultText(ran)
    if (text === undefined) return ran
    let payload: unknown
    try {
      payload = JSON.parse(text)
    } catch {
      return ran
    }
    await remember($, tool, payload)
    return ran
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const rows = e.props.scroll?.bodyRows ?? e.viewport?.rows ?? Number.POSITIVE_INFINITY
    const lines = paneLines(
      await read($, groups),
      await read($, droppedGroups),
      await read($, overlay),
      Math.max(rows, 1),
    )
    return (
      <Box flexDirection="column">
        {lines.map(line => (
          <Box key={line.key}>
            <Text dimColor={line.isDim === true} bold={line.isBold === true} wrap="truncate-end">
              {line.text}
              {line.detail ? <Text dimColor>{line.detail}</Text> : null}
            </Text>
          </Box>
        ))}
      </Box>
    )
  })
}
