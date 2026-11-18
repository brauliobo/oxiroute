import type { TopologyEdge, TopologyEdgeKind, TopologyNode, TopologyNodeKind } from '../api'

const TLS_EDGES: readonly TopologyEdgeKind[] = ['listener_tls', 'tls_certificate']

export const NODE_WIDTH = 168
export const NODE_HEIGHT = 112
const COLUMN_GAP = 32
const ROW_GAP = 148
const PADDING = 16
const TOP_PADDING = 24
const COLUMNS = 5

const KIND_COLUMN: Record<TopologyNodeKind, number> = {
  listener: 0,
  forward_proxy_listener: 0,
  rtmp_listener: 0,
  forward_proxy_service: 1,
  tls_profile: 1,
  http_service: 1,
  l4_service: 1,
  certificate: 2,
  http_route: 2,
  upstream_pool: 3,
  endpoint: 4,
}

export interface Position {
  x: number
  y: number
}

export interface Connector {
  id: string
  tls: boolean
  path: string
}

export interface Layout {
  orderedNodes: TopologyNode[]
  positions: Map<string, Position>
  width: number
  height: number
  connectors: Connector[]
}

const columnX = (column: number) => PADDING + column * (NODE_WIDTH + COLUMN_GAP)

function connectorPath(from: Position, to: Position): string {
  const startX = from.x + NODE_WIDTH
  const startY = from.y + NODE_HEIGHT / 2
  const endX = to.x
  const endY = to.y + NODE_HEIGHT / 2
  const bend = Math.max(28, (endX - startX) / 2)
  return `M ${startX} ${startY} C ${startX + bend} ${startY}, ${endX - bend} ${endY}, ${endX} ${endY}`
}

export function layoutTopology(nodes: readonly TopologyNode[], edges: readonly TopologyEdge[]): Layout {
  const orderedNodes = nodes
    .map((node, sourceIndex) => ({ node, sourceIndex }))
    .sort((left, right) => KIND_COLUMN[left.node.kind] - KIND_COLUMN[right.node.kind] || left.sourceIndex - right.sourceIndex)
    .map(({ node }) => node)
  const rows = Array.from({ length: COLUMNS }, () => 0)
  const positions = new Map<string, Position>()
  for (const node of orderedNodes) {
    const column = KIND_COLUMN[node.kind]
    positions.set(node.id, { x: columnX(column), y: TOP_PADDING + rows[column]! * ROW_GAP })
    rows[column]! += 1
  }
  const connectors = edges.flatMap(({ id, kind, source, target }) => {
    const from = positions.get(source)
    const to = positions.get(target)
    return from && to ? [{ id, tls: TLS_EDGES.includes(kind), path: connectorPath(from, to) }] : []
  })
  const height = Math.max(NODE_HEIGHT + 2 * TOP_PADDING, TOP_PADDING * 2 + Math.max(...rows) * ROW_GAP - (ROW_GAP - NODE_HEIGHT))
  return { orderedNodes, positions, width: columnX(COLUMNS) - COLUMN_GAP + PADDING, height, connectors }
}
