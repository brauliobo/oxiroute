import type { TopologyNode, TopologyRuntimeOverlay } from '../api'
import { formatBytes, formatCount } from '../formatters'

const LISTENER_KINDS: readonly TopologyNode['kind'][] = ['listener', 'forward_proxy_listener', 'rtmp_listener']

export interface ConfiguredLimit {
  label: string
  value: string
}

export function identityLabel(node: TopologyNode): string {
  if (LISTENER_KINDS.includes(node.kind)) {
    const bind = node.attributes.bind
    if (!bind) return ''
    if (bind.type === 'unix') return `Unix / ${bind.path}`
    return `${bind.type === 'udp' ? 'UDP' : 'Socket'} / ${bind.address}`
  }
  if (node.kind !== 'endpoint') return ''
  const { type, address, host, port, path } = node.attributes
  if (type === 'socket' && typeof address === 'string') return `Socket / ${address}`
  if (type === 'dns' && typeof host === 'string' && typeof port === 'number') return `DNS / ${host}:${port}`
  if (type === 'unix' && typeof path === 'string') return `Unix / ${path}`
  return ''
}

const limit = (value: unknown, format: (limit: number) => string): string =>
  value === null ? 'Unbounded' : typeof value === 'number' ? format(value) : 'Unknown'

export function configuredLimit(node: TopologyNode): ConfiguredLimit | null {
  if (LISTENER_KINDS.includes(node.kind) && 'maxConnections' in node.attributes) {
    return { label: 'Connection limit', value: limit(node.attributes.maxConnections, formatCount) }
  }
  if (node.kind === 'http_service' && 'maxRequestBodyBytes' in node.attributes) {
    return { label: 'Request body limit', value: limit(node.attributes.maxRequestBodyBytes, formatBytes) }
  }
  return null
}

export function activeConnections(overlay: TopologyRuntimeOverlay | undefined): string | null {
  const value = overlay?.metrics.activeConnections
  if (typeof value === 'number') return Number.isSafeInteger(value) && value >= 0 ? String(value) : null
  return typeof value === 'string' && /^(0|[1-9][0-9]*)$/.test(value) ? value : null
}
