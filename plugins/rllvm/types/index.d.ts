// What the overlay view keeps in `$.state`: the last answers of
// rllvm-query's MCP tools, reduced to what the pane draws. Nothing here is
// read from disk; unsaved records show because they arrive in tool results.

/** A function as rllvm-query names it: the module it is defined in, and its symbol. */
export type OverlayFunction = { module_id: string; symbol: string }

/** A struct field by record name and byte offset (`ops@8`). */
export type OverlayField = { record: string; offset: number }

/** One unresolved indirect call site. */
export type OverlaySite = {
  function: OverlayFunction
  block_index: number
  instruction_index: number
}

/** A site and where it is in the source, when the build recorded that. */
export type OverlaySiteAt = { site: OverlaySite; location: string | null }

/** One `resolution_candidates` group: a field (or a set of sites) and the functions it may call. */
export type OverlayGroup = {
  field: OverlayField | null
  field_name: string | null
  signature: string
  sites: OverlaySiteAt[]
  candidates: OverlayFunction[]
  single_candidate: boolean
}

/** An overlay edge's identity: a field or a single site, and the function it calls. */
export type OverlayEdgeKey =
  { via_field: OverlayField; to: OverlayFunction } | { site: OverlaySite; to: OverlayFunction }

export type OverlayConfidence = 'low' | 'medium' | 'high'

export type OverlayVerdict = 'confirmed' | 'refuted' | 'inconclusive'

/** One agent-recorded edge and what has been checked about it. */
export type OverlayEdge = {
  key: OverlayEdgeKey
  confidence: OverlayConfidence
  verdict: OverlayVerdict | null
  sites: number
}

/** The overlay as the last summary reported it. */
export type OverlayView = {
  path: string | null
  pending: number
  edges: OverlayEdge[]
  unresolved_sites: number
  covered_sites: number
}

declare module 'claude-code' {
  interface PluginState {
    rllvm: {
      groups: OverlayGroup[]
      overlay: OverlayView | null
    }
  }
}
