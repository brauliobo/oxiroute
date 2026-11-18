import { CANONICAL_FIELD_REGISTRY } from './fieldRegistry'
import type { CanonicalConfig } from './model'

export interface DiagnosticLocation {
  selection: string
  field: string | null
  nested: number[]
}

const COLLECTIONS: readonly string[] = [
  'certificates',
  'tls_profiles',
  'listeners',
  'cache_stores',
  'upstream_pools',
  'http_services',
  'forward_proxy_services',
  'rtmp_services',
  'l4_services',
]

const isRegisteredItemField = (prefix: string): boolean =>
  CANONICAL_FIELD_REGISTRY.some(({ path }) => path.startsWith(`${prefix}.`))

const isObject = (node: unknown): node is Record<string, unknown> =>
  typeof node === 'object' && node !== null && !Array.isArray(node)

const decodeSegment = (segment: string): string => segment.replaceAll('~1', '/').replaceAll('~0', '~')

function itemIndex(items: unknown[], segment: string): number {
  if (/^\d+$/.test(segment)) return Number(segment) < items.length ? Number(segment) : -1
  return items.findIndex((item) => isObject(item) && item.name === segment)
}

function selectionFor(root: string, collectionIndex: number | undefined): string {
  if (COLLECTIONS.includes(root)) return `${root}:${collectionIndex}`
  return root === 'management' || root === 'stats' ? root : 'general'
}

export function locateDiagnostic(config: CanonicalConfig, pointer: string): DiagnosticLocation | null {
  const normalized = pointer.startsWith('/') ? pointer : pointer.replace(/\[(\w+)\]/g, '/$1').replaceAll('.', '/')
  const queue = normalized.split('/').filter(Boolean).map(decodeSegment)
  if (queue[0] === 'config') queue.shift()
  const root = queue.shift()
  if (root === undefined) return null
  const indexes: number[] = []
  let field = root
  let node: unknown = (config as unknown as Record<string, unknown>)[root]

  while (queue.length > 0) {
    const segment = queue.shift()!
    if (Array.isArray(node)) {
      const index = itemIndex(node, segment)
      if (index < 0) break
      node = node[index]
      if (!isObject(node)) break
      field += '[]'
      indexes.push(index)
    } else if (node === undefined && /^\d+$/.test(segment) && isRegisteredItemField(`${field}[]`)) {
      field += '[]'
      indexes.push(Number(segment))
    } else if (/^\d+$/.test(segment)) {
      break
    } else if (isObject(node) && !(segment in node) && segment.includes('.')) {
      queue.unshift(...segment.split('.'))
    } else {
      field += `.${segment}`
      node = isObject(node) ? node[segment] : undefined
    }
  }

  const collection = COLLECTIONS.includes(root)
  if (collection && indexes.length === 0) return null
  return {
    selection: selectionFor(root, indexes[0]),
    field: field === `${root}[]` ? null : field,
    nested: collection ? indexes.slice(1) : indexes,
  }
}
