import { describe, expect, it } from 'vitest'
import { counter, wirePool, wireServer } from '../fixtures'
import { documentAt, loadedStore } from './testing'
import { summarizeOverview } from './overview'

const traffic = (seconds: number, requests: number, failing: number) => documentAt(seconds, (wire) => {
  const listener = wire.listeners[0]!
  listener.http!.total = counter(requests)
  listener.http!.outcomes = [{ result: 'success', count: counter(requests - failing) }, { result: 'timeout', count: counter(failing) }]
  wire.process.bytesReceived = counter(1000 * requests)
  wire.process.bytesSent = counter(4000 * requests)
})

describe('summarizeOverview', () => {
  it('has no rates after the first sample and keeps the counts that need none', async () => {
    const { store } = await loadedStore([traffic(0, 100, 0)])
    const overview = summarizeOverview(store.model.value!)
    expect(overview).toMatchObject({
      version: '6.3.0', uptimeMs: 3_600_000, requestRate: undefined, bytesInRate: undefined, errorPercent: undefined,
      generation: { number: 4, accepting: true, activeRevision: 'rev-4' },
      connections: { active: 7, max: 4096, accepted: 5000n },
      pools: { up: 1, total: 2, degraded: 1, down: 0, empty: 0 }, servers: { up: 2, total: 3 }, warnings: 1,
    })
  })

  it('sums request rates, derives the error share and the byte rates', async () => {
    const harness = await loadedStore([traffic(0, 100, 0), traffic(2, 300, 20)])
    await harness.next()
    const overview = summarizeOverview(harness.store.model.value!)
    expect(overview.requestRate).toBe(100)
    expect(overview.errorPercent).toBe(10)
    expect(overview.bytesInRate).toBe(100_000)
    expect(overview.bytesOutRate).toBe(400_000)
  })

  it('has no error share without traffic, and no certificate when none has an expiry', async () => {
    const harness = await loadedStore([traffic(0, 100, 0), traffic(2, 100, 0)])
    await harness.next()
    expect(summarizeOverview(harness.store.model.value!).errorPercent).toBeUndefined()
    const none = await loadedStore([documentAt(0, (wire) => { wire.certificates = [] })])
    expect(summarizeOverview(none.store.model.value!).certificate).toBeUndefined()
  })

  it('names the certificate that expires first and counts pools by status', async () => {
    const { store } = await loadedStore([documentAt(0, (wire) => {
      wire.certificates.push({ name: 'old', source: 'certbot', expiresAt: 'x', notAfterUnixSeconds: wire.sampledAtUnixMs / 1000 + 3 * 86_400 })
      wire.certificates.push({ name: 'unreadable', source: 'certbot', expiresAt: 'x' })
      wire.pools = [wirePool('a', [wireServer('s', '198.51.100.1:1', { available: false, status: 'down' })]), wirePool('b', [])]
    })])
    const overview = summarizeOverview(store.model.value!)
    expect(overview.certificate).toMatchObject({ name: 'old', severity: 'soon' })
    expect(overview.pools).toMatchObject({ up: 0, down: 1, empty: 1, degraded: 0, total: 2 })
  })
})
