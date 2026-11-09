import { describe, expect, it } from 'vitest'
import { counter, wireDocument, wireServer } from './fixtures'
import { DecodeError, SCHEMA_VERSION, UnsupportedSchemaError, decodeSnapshot, safeNumber } from './decode'

const firstServer = (document: ReturnType<typeof decodeSnapshot>) => document.pools[0]!.servers[0]!

describe('decodeSnapshot', () => {
  it('turns decimal-string counters into bigints and leaves gauges as numbers', () => {
    const document = decodeSnapshot(wireDocument())
    expect(firstServer(document).traffic.sessions).toEqual({ current: 3, total: 1000n })
    expect(document.process.acceptedConnections).toBe(5000n)
    expect(document.listeners[0]!.http!.outcomes[0]).toEqual({ result: 'success', count: 4800n })
    expect(document.listeners[0]!.http!.latency.buckets.every((bucket) => typeof bucket === 'bigint')).toBe(true)
    expect(document.bucketBoundsMs.operation[0]).toBe(1)
    expect(document.process.cpuPercent).toBe(12.5)
  })

  it('keeps u64 counters exact beyond the safe integer range', () => {
    const wire = wireDocument()
    wire.process.bytesSent = '18446744073709551615'
    expect(decodeSnapshot(wire).process.bytesSent).toBe(18446744073709551615n)
  })

  it('does not mistake numeric-looking names and messages for counters', () => {
    const wire = wireDocument()
    wire.pools[0]!.servers[0] = wireServer('123', '198.51.100.11:8080')
    wire.warnings = [{ code: '42', message: '7' }]
    const document = decodeSnapshot(wire)
    expect(firstServer(document).name).toBe('123')
    expect(document.warnings[0]).toEqual({ code: '42', message: '7' })
  })

  it('keeps absent fields absent and never fabricates zeros', () => {
    const wire = wireDocument()
    delete wire.process.cpuPercent
    delete wire.listeners[0]!.http
    delete wire.pools[0]!.servers[0]!.lastTransitionAtUnixMs
    const document = decodeSnapshot(wire)
    expect('cpuPercent' in document.process).toBe(false)
    expect('http' in document.listeners[0]!).toBe(false)
    expect('cache' in document.listeners[0]!).toBe(false)
    expect('lastTransitionAtUnixMs' in firstServer(document)).toBe(false)
    expect('tcpRelays' in document.listeners[0]!).toBe(false)
  })

  it('does not modify its input', () => {
    const wire = wireDocument()
    const copy = structuredClone(wire)
    decodeSnapshot(wire)
    expect(wire).toEqual(copy)
  })

  it('decodes the JSON text a server sends', () => {
    const document = decodeSnapshot(JSON.parse(JSON.stringify(wireDocument())))
    expect(document.schemaVersion).toBe(SCHEMA_VERSION)
  })

  it('rejects another schema version before reading anything else', () => {
    const error = catching(() => decodeSnapshot({ schemaVersion: 2, surprise: true }))
    expect(error).toBeInstanceOf(UnsupportedSchemaError)
    expect((error as UnsupportedSchemaError).version).toBe(2)
    expect(catching(() => decodeSnapshot({ pools: [] }))).toBeInstanceOf(UnsupportedSchemaError)
  })

  it.each(['12x', '-1', '', '1.5', ' 7'])('rejects the malformed counter %j with its path', (value) => {
    const wire = wireDocument()
    wire.pools[0]!.servers[0]!.traffic.sessions.total = value as never
    const error = catching(() => decodeSnapshot(wire)) as DecodeError
    expect(error).toBeInstanceOf(DecodeError)
    expect(error.path).toBe('pools[0].servers[0].traffic.sessions.total')
  })

  it('rejects a JSON number where a counter string is required', () => {
    const wire = wireDocument()
    wire.process.retryAttempts = 10 as never
    expect((catching(() => decodeSnapshot(wire)) as DecodeError).path).toBe('process.retryAttempts')
  })

  it.each([null, 'text', 7, []])('rejects the non-document %j', (value) => {
    expect(catching(() => decodeSnapshot(value))).toBeInstanceOf(DecodeError)
  })

  it.each(['listeners', 'routes', 'pools', 'certificates', 'certificateWatchers', 'warnings'] as const)(
    'rejects a document whose %s is not a list', (section) => {
      const wire = { ...wireDocument(), [section]: {} }
      expect((catching(() => decodeSnapshot(wire)) as DecodeError).path).toBe(section)
    })

  it.each(['process', 'generation', 'bucketBoundsMs'] as const)('rejects a document without %s', (section) => {
    const wire: Record<string, unknown> = { ...wireDocument() }
    delete wire[section]
    expect((catching(() => decodeSnapshot(wire)) as DecodeError).path).toBe(section)
  })

  it.each(['sampledAtUnixMs', 'intervalHintMs'] as const)('rejects a document without a numeric %s', (key) => {
    const wire = { ...wireDocument(), [key]: '5' }
    expect((catching(() => decodeSnapshot(wire)) as DecodeError).path).toBe(key)
  })
})

describe('safeNumber', () => {
  it('is exact up to the safe integer range and approximate above it', () => {
    expect(safeNumber(0n)).toBe(0)
    expect(safeNumber(BigInt(Number.MAX_SAFE_INTEGER))).toBe(Number.MAX_SAFE_INTEGER)
    expect(safeNumber(2n ** 64n - 1n)).toBe(2 ** 64)
  })

  it('reads counters built by the fixtures', () => {
    expect(safeNumber(BigInt(counter(12)))).toBe(12)
  })
})

function catching(action: () => unknown): unknown {
  try {
    action()
  } catch (error) {
    return error
  }
  throw new Error('expected the call to throw')
}
