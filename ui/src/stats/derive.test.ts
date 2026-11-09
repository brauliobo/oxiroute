import { describe, expect, it } from 'vitest'
import { decodeSnapshot } from './decode'
import { deriveModel } from './derive'
import { wireDocument } from './fixtures'
import { buildModel } from './model'

const base = () => buildModel(decodeSnapshot(wireDocument()))

describe('deriveModel', () => {
  it('attaches the rates of a row by its key and leaves unknown ones out', () => {
    const rates = new Map([['server:web/origin-a#requests', 12.5], ['listener:public-http#bytesSent', 1000]])
    const derived = deriveModel(base(), rates)
    expect(derived.pools[0]!.servers[0]!.rates).toEqual({ requests: 12.5 })
    expect(derived.listeners[0]!.rates).toEqual({ bytesSent: 1000 })
    expect(derived.pools[1]!.rates).toEqual({})
    expect(derived.process.rates).toEqual({})
  })

  it('summarises latency per histogram with the matching bucket bounds', () => {
    const derived = deriveModel(base(), new Map())
    const server = derived.pools[0]!.servers[0]!
    expect(server.latency.request).toMatchObject({ count: 10n, p50: { ms: 50, overflow: false }, p99: { ms: 5000, overflow: false } })
    expect(server.latency.header!.p50.ms).toBe(50)
    expect(derived.pools[0]!.latency.wait).toMatchObject({ count: 4n })
    expect(derived.listeners[0]!.latency.request!.count).toBe(10n)
  })

  it('has no latency summary for a histogram without observations', () => {
    const wire = wireDocument()
    const histogram = wire.pools[0]!.servers[0]!.latency.request
    histogram.count = '0'
    histogram.buckets = histogram.buckets.map(() => '0')
    const server = deriveModel(buildModel(decodeSnapshot(wire)), new Map()).pools[0]!.servers[0]!
    expect('request' in server.latency).toBe(false)
    expect('header' in server.latency).toBe(true)
  })

  it('keeps everything else of the model as it is', () => {
    const model = base()
    const derived = deriveModel(model, new Map())
    expect(derived.summary).toBe(model.summary)
    expect(derived.pools[0]!.servers[0]!.key).toBe('server:web/origin-a')
    expect(derived.routes).toBe(model.routes)
  })
})
