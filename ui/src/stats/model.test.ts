import { describe, expect, it } from 'vitest'
import { decodeSnapshot } from './decode'
import { counter, wireDocument, wirePool, wireServer } from './fixtures'
import type { StatsDocument } from './generated/statsPage'
import { CERT_SOON_MS, STATUS_LABELS, buildModel, modelCounters, modelGauges, rowKey } from './model'

const model = (overrides: Partial<StatsDocument> = {}) => buildModel(decodeSnapshot(wireDocument(overrides)))

describe('row keys', () => {
  it('are prefixed by kind and escape separators so that names cannot collide', () => {
    expect(rowKey('server', 'web', 'origin-a')).toBe('server:web/origin-a')
    expect(rowKey('server', 'a/b', 'c')).toBe('server:a%2Fb/c')
    expect(rowKey('server', 'a', 'b/c')).not.toBe(rowKey('server', 'a/b', 'c'))
    expect(rowKey('pool', 'a&b=c d')).toBe('pool:a%26b%3Dc%20d')
  })

  it('stay the same when the server reorders pools, servers or listeners', () => {
    const wire = wireDocument()
    const keys = (document: StatsDocument) => {
      const built = buildModel(decodeSnapshot(document))
      return [...built.pools.flatMap((pool) => [pool.key, ...pool.servers.map((server) => server.key)]),
        ...built.listeners.map((listener) => listener.key)].sort()
    }
    const reordered = structuredClone(wire)
    reordered.pools.reverse()
    reordered.pools.forEach((pool) => pool.servers.reverse())
    expect(keys(reordered)).toEqual(keys(wire))
  })

  it('separate same-named servers of different pools', () => {
    const wire = wireDocument({ pools: [wirePool('web', [wireServer('origin-a', '198.51.100.11:80')]),
      wirePool('api', [wireServer('origin-a', '198.51.100.21:80')])] })
    const keys = buildModel(decodeSnapshot(wire)).pools.flatMap((pool) => pool.servers.map((server) => server.key))
    expect(new Set(keys).size).toBe(2)
  })
})

describe('buildModel', () => {
  it('lists listeners, routes, pools with their servers, certificates and warnings', () => {
    const built = model()
    expect(built.listeners.map((row) => row.key)).toEqual(['listener:public-http'])
    expect(built.routes.map((row) => row.key)).toEqual(['route:site%230'])
    expect(built.pools.map((pool) => pool.name)).toEqual(['web', 'api'])
    expect(built.pools[0]!.servers.map((server) => [server.key, server.pool])).toEqual(
      [['server:web/origin-a', 'web'], ['server:web/origin-b', 'web']])
    expect(built.certificates[0]!.key).toBe('certificate:edge')
    expect(built.warnings.map((row) => row.key)).toEqual(['warning:W_PASSIVE_HEALTH_MISSING'])
  })

  it('numbers repeated warning codes', () => {
    const built = model({ warnings: [{ code: 'W_A', message: 'one' }, { code: 'W_A', message: 'two' }] })
    expect(built.warnings.map((row) => row.key)).toEqual(['warning:W_A', 'warning:W_A#2'])
  })

  it('keeps the decoded wire data of a row and the sample metadata', () => {
    const built = model({ admin: true })
    expect(built.admin).toBe(true)
    expect(built.sampledAtUnixMs).toBe(1_800_000_000_000)
    expect(built.intervalHintMs).toBe(2000)
    expect(built.bucketBoundsMs.operation[0]).toBe(1)
    expect(built.pools[0]!.servers[0]!.wire.traffic.sessions.total).toBe(1000n)
    expect(built.generation.wire.activeRevision).toBe('rev-4')
  })

  it('labels statuses in words so that colour is never the only signal', () => {
    expect(STATUS_LABELS).toMatchObject({ up: 'UP', down: 'DOWN', drain: 'DRAIN', maint: 'MAINT', ejected: 'EJECTED' })
    expect(model().pools[0]!.servers[1]!.statusLabel).toBe('DRAIN')
  })

  it('counts what is up and what needs attention', () => {
    const built = model()
    expect(built.summary).toEqual({
      listeners: { up: 1, total: 1 }, pools: { up: 1, total: 2 }, servers: { up: 2, total: 3 }, problems: 2, warnings: 1,
    })
  })
})

describe('problems', () => {
  const server = (status: Parameters<typeof wireServer>[2] & object) => {
    const built = model({ pools: [wirePool('web', [wireServer('origin-a', '198.51.100.11:80', status)])] })
    return built.pools[0]!.servers[0]!.problem
  }

  it.each([['down', true], ['ejected', true], ['unknown', true], ['drain', true], ['maint', true], ['up', false],
    ['unchecked', false]] as const)('a %s server is a problem: %s', (status, expected) => {
    expect(server({ status })).toBe(expected)
  })

  it('marks a pool that is not fully up, and a listener that is not listening', () => {
    const built = model({ listeners: [{ ...wireDocument().listeners[0]!, state: 'failed' }] })
    expect(built.listeners[0]!.problem).toBe(true)
    expect(built.pools[0]!.problem).toBe(true)
    expect(built.pools[1]!.problem).toBe(false)
  })

  it('marks a degraded process or generation and an unhealthy watcher', () => {
    const wire = wireDocument()
    wire.process.state = 'degraded'
    wire.generation.degraded = true
    wire.certificateWatchers = [{ source: 'certbot', health: 'degraded', backendErrors: counter(1), reconciliationFailures: counter(0) }]
    const built = buildModel(decodeSnapshot(wire))
    expect([built.process.problem, built.generation.problem, built.certificateWatchers[0]!.problem]).toEqual([true, true, true])
  })

  it('does not treat an unsupported host as a problem', () => {
    const wire = wireDocument()
    wire.process.host.state = 'unsupported'
    expect(buildModel(decodeSnapshot(wire)).process.problem).toBe(false)
  })
})

describe('certificates', () => {
  const at = (secondsFromSample: number | undefined) => {
    const wire = wireDocument()
    const certificate = wire.certificates[0]!
    if (secondsFromSample === undefined) delete certificate.notAfterUnixSeconds
    else certificate.notAfterUnixSeconds = wire.sampledAtUnixMs / 1000 + secondsFromSample
    return buildModel(decodeSnapshot(wire)).certificates[0]!
  }

  it('measure the remaining validity against the server clock of the sample', () => {
    expect(at(86_400)).toMatchObject({ remainingMs: 86_400_000, severity: 'soon' })
    expect(at(CERT_SOON_MS / 1000 + 1)).toMatchObject({ severity: 'ok' })
    expect(at(-5)).toMatchObject({ remainingMs: -5000, severity: 'expired' })
  })

  it('stay unknown, not expired, when the expiry could not be read', () => {
    const certificate = at(undefined)
    expect('remainingMs' in certificate).toBe(false)
    expect(certificate.severity).toBe('unknown')
  })

  it('are problems when expired or close to expiry', () => {
    expect([at(-1).problem, at(3600).problem, at(CERT_SOON_MS / 1000 * 2).problem, at(undefined).problem])
      .toEqual([true, true, false, false])
  })
})

describe('modelCounters', () => {
  it('flattens the counters of every row under row-key#metric and skips what the server did not send', () => {
    const wire = wireDocument()
    delete wire.listeners[0]!.http
    const counters = modelCounters(buildModel(decodeSnapshot(wire)))
    expect(counters.get('server:web/origin-a#requests')).toBe(1000n)
    expect(counters.get('server:web/origin-a#bytesReceived')).toBe(2_000_000n)
    expect(counters.get('listener:public-http#bytesSent')).toBe(64_000_000n)
    expect(counters.get('process#accepted')).toBe(5000n)
    expect(counters.has('listener:public-http#requests')).toBe(false)
  })

  it('counts errors as failed attempts plus 5xx answers of a server and the failing outcomes of a listener', () => {
    const counters = modelCounters(model())
    expect(counters.get('server:web/origin-a#errors')).toBe(10n + 5n + 5n + 20n)
    expect(counters.get('listener:public-http#errors')).toBe(30n + 10n)
  })

  it('sums the pool from the totals the server computed', () => {
    const counters = modelCounters(model())
    expect(counters.get('pool:web#requests')).toBe(2000n)
  })
})

describe('modelGauges', () => {
  it('lists the open connections of the process, listeners, pools and servers', () => {
    const gauges = modelGauges(model())
    expect(gauges.get('process#active')).toBe(7)
    expect(gauges.get('listener:public-http#active')).toBe(7)
    expect(gauges.get('server:web/origin-a#active')).toBe(3)
    expect(gauges.get('pool:web#active')).toBe(6)
  })
})
