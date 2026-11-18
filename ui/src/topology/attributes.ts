import { formatBytes, formatCount, formatTelemetryDuration, formatTime } from '../formatters'
import type { Metric } from '../overview/metric'

const SENSITIVE_KEYS = new Set(['rootDirectory', 'root_directory', 'tokenFilePath', 'token_file_path'])
const ACRONYMS = new Set(['io', 'tls', 'dns', 'tcp', 'udp', 'http', 'https', 'id', 'ca', 'ip', 'sni', 'alpn', 'ttl', 'url', 'uri'])
const PLAIN_NUMBERS = new Set(['port', 'version', 'revision'])
const INTEGER = /^(0|[1-9][0-9]*)$/
const NONE = 'None'

type Leaf = [path: string[], value: unknown]

const words = (key: string): string[] => key.match(/[A-Z]+(?![a-z])|[A-Z]?[a-z0-9]+/g)?.map((word) => word.toLowerCase()) ?? [key]
const withoutUnit = (tokens: string[]): string[] => (tokens.at(-1) === 'ms' ? tokens.filter((token) => token !== 'ms' && token !== 'unix') : tokens)

function title(tokens: string[]): string {
  const text = tokens.map((token) => (ACRONYMS.has(token) ? token.toUpperCase() : token)).join(' ')
  return text.charAt(0).toUpperCase() + text.slice(1)
}

export function suppressSensitivePaths(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(suppressSensitivePaths)
  if (typeof value !== 'object' || value === null) return value
  const entries = Object.entries(value).filter(([key]) => !SENSITIVE_KEYS.has(key))
  return Object.fromEntries(entries.map(([key, entry]) => [key, suppressSensitivePaths(entry)]))
}

function leaves(value: unknown, path: string[]): Leaf[] {
  if (Array.isArray(value)) {
    if (value.every((item) => typeof item !== 'object' || item === null)) return [[path, value]]
    return value.flatMap((item, index) => leaves(item, [...path, String(index + 1)]))
  }
  if (typeof value === 'object' && value !== null) {
    return Object.entries(value).flatMap(([key, entry]) => leaves(entry, [...path, key]))
  }
  return [[path, value]]
}

function formatScalar(tokens: string[], value: unknown): string {
  if (value === null || value === undefined) return NONE
  if (typeof value === 'boolean') return value ? 'Yes' : 'No'
  const counter = typeof value === 'number' || (typeof value === 'string' && INTEGER.test(value))
  if (!counter) return String(value)
  const last = tokens.at(-1)!
  if (tokens.includes('bytes')) return formatBytes(value as number | string)
  if (last === 'ms' && tokens.includes('unix')) return Number(value) === 0 ? NONE : formatTime(Number(value))
  if (last === 'ms') return formatTelemetryDuration(Number(value))
  return PLAIN_NUMBERS.has(last) ? String(value) : formatCount(value as number | string)
}

function formatLeaf(tokens: string[], value: unknown): string {
  if (!Array.isArray(value)) return formatScalar(tokens, value)
  return value.length === 0 ? NONE : value.map((item) => formatScalar(tokens, item)).join(', ')
}

export function describeAttributes(attributes: Record<string, unknown>): Metric[] {
  return leaves(suppressSensitivePaths(attributes), []).filter(([path]) => path.length > 0).map(([path, value]) => {
    const segments = path.map(words)
    const label = segments.map((tokens, index) => title(index === segments.length - 1 ? withoutUnit(tokens) : tokens)).join(' / ')
    return { label, value: formatLeaf(segments.at(-1)!, value) }
  })
}
