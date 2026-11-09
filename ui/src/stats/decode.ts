import { statsPageCounterShape } from './generated/statsPage'
import type { DecimalCounter, StatsDocument } from './generated/statsPage'

export const SCHEMA_VERSION = 1

/** The wire document with every decimal-string counter read as a bigint; absent keys stay absent. */
export type Decoded<T> = T extends DecimalCounter
  ? bigint
  : T extends readonly (infer Item)[]
    ? Decoded<Item>[]
    : T extends object
      ? { [Key in keyof T]: Decoded<T[Key]> }
      : T
export type StatsSnapshot = Decoded<StatsDocument>

/** The document is not a snapshot of this contract; `path` is the JSON path of the offending value. */
export class DecodeError extends Error {
  readonly path: string

  constructor(path: string, message: string) {
    super(`${path || 'document'}: ${message}`)
    this.name = 'DecodeError'
    this.path = path
  }
}

/** The server speaks another `schemaVersion` than this build understands; reload to get the matching page. */
export class UnsupportedSchemaError extends Error {
  readonly version: unknown

  constructor(version: unknown) {
    super(`unsupported stats schemaVersion ${JSON.stringify(version)} (this page reads ${SCHEMA_VERSION})`)
    this.name = 'UnsupportedSchemaError'
    this.version = version
  }
}

const SECTION_LISTS = ['listeners', 'routes', 'pools', 'certificates', 'certificateWatchers', 'warnings'] as const
const SECTION_OBJECTS = ['process', 'generation', 'bucketBoundsMs'] as const
const COUNTER = /^[0-9]+$/

/**
 * Checks `schemaVersion` first, then the document skeleton the view model reads (numbers, section lists and
 * objects), and converts counters by the generated shape table. Values deeper than the skeleton are trusted to the
 * contract except counters, which must be decimal strings. The input is not modified.
 */
export function decodeSnapshot(input: unknown): StatsSnapshot {
  if (!isRecord(input)) throw new DecodeError('', 'not a JSON object')
  if (input.schemaVersion !== SCHEMA_VERSION) throw new UnsupportedSchemaError(input.schemaVersion)
  for (const key of ['sampledAtUnixMs', 'intervalHintMs'] as const) {
    if (typeof input[key] !== 'number') throw new DecodeError(key, 'expected a number')
  }
  for (const key of SECTION_LISTS) if (!Array.isArray(input[key])) throw new DecodeError(key, 'expected a list')
  for (const key of SECTION_OBJECTS) if (!isRecord(input[key])) throw new DecodeError(key, 'expected an object')
  return convert('StatsDocument', input, '') as StatsSnapshot
}

/** Exact for counters up to 2^53 - 1; above that the nearest double, which is enough to display and to rate. */
export const safeNumber = (value: bigint): number => Number(value)

function convert(schema: string, value: unknown, path: string): unknown {
  if (!isRecord(value)) throw new DecodeError(path, 'expected an object')
  const kinds = statsPageCounterShape[schema] ?? {}
  const result: Record<string, unknown> = {}
  for (const [key, member] of Object.entries(value)) {
    const kind = kinds[key]
    result[key] = kind === undefined ? member : convertKind(kind, member, path ? `${path}.${key}` : key)
  }
  return result
}

function convertKind(kind: string, value: unknown, path: string): unknown {
  if (kind.endsWith('[]')) {
    if (!Array.isArray(value)) throw new DecodeError(path, 'expected a list')
    return value.map((item, index) => convertKind(kind.slice(0, -2), item, `${path}[${index}]`))
  }
  return kind === 'c' ? counterValue(value, path) : convert(kind, value, path)
}

function counterValue(value: unknown, path: string): bigint {
  if (typeof value !== 'string' || !COUNTER.test(value)) throw new DecodeError(path, 'expected a decimal-string counter')
  return BigInt(value)
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)
