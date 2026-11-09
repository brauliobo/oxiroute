import type { Page } from '@playwright/test'

import { BASE_SAMPLED_AT_MS, counter, wireDocument, wireListener, wirePool, wireServer } from '../../../src/stats/fixtures'
import type { StatsDocument } from '../../../src/stats/generated/statsPage'

// Invented data for the dashboard specs: documentation addresses, pools `web`, `api` and `files`. `seconds` is the
// time since the first sample; counters grow with it, so rates and sparklines appear from the second sample on.

export const HOSTILE_POOL = '<img src=x onerror="window.xss=1">'
export const HOSTILE_SERVER = '<script>window.xss=1</script>'

export function dashboardDocument(seconds = 0, edit: (wire: StatsDocument) => void = () => {}): StatsDocument {
  const grow = (base: number, perSecond: number) => counter(base + perSecond * seconds)
  const busy = wireServer('origin-a', '198.51.100.11:8080', { weight: 5, configuredWeight: 10 })
  busy.traffic.sessions.total = grow(1000, 100)
  busy.traffic.bytesSent = grow(500_000, 50_000)
  busy.traffic.attempts.connectFailed = grow(10, 5)
  const down = wireServer('origin-b', '198.51.100.12:8080', {
    status: 'down', health: 'unhealthy', available: false, downtimeMs: 125_000,
    lastTransitionAtUnixMs: BASE_SAMPLED_AT_MS - 125_000,
  })
  down.check.consecutiveFailures = 4
  down.check.lastFailure = 'timeout'
  const draining = wireServer('origin-c', '198.51.100.13:8080', { status: 'drain', administrativeState: 'drain', available: false })
  const maint = wireServer('origin-d', '198.51.100.14:8080', { status: 'maint', administrativeState: 'maintenance', available: false })
  const ejected = wireServer('origin-e', '198.51.100.15:8080', { status: 'ejected', available: false })
  ejected.passive = { ...ejected.passive, ejected: true, ejectionReason: 'timeout', ejectionCount: counter(2) }
  const unchecked = wireServer('origin-f', '198.51.100.16:8080', { status: 'unchecked', health: 'unchecked' })
  unchecked.check.enabled = false
  const wire = wireDocument({
    sampledAtUnixMs: BASE_SAMPLED_AT_MS + seconds * 1000,
    intervalHintMs: 1000,
    pools: [
      wirePool('web', [busy, down, draining, maint, ejected, unchecked]),
      wirePool('api', [wireServer('origin-g', '198.51.100.21:9000', { origin: 'dynamic' })]),
      wirePool(HOSTILE_POOL, [wireServer(HOSTILE_SERVER, '198.51.100.31:1')]),
    ],
    listeners: [
      wireListener('public-http'),
      wireListener('public-tls', { bind: 'socket:192.0.2.10:8443', tlsProfile: 'modern' }),
      wireListener('relay', { protocol: 'tcp', state: 'failed', administrativeState: 'drain', maxConnections: undefined }),
    ],
  })
  wire.listeners[0]!.http!.total = grow(4900, 100)
  wire.certificates = [
    { name: 'edge', source: 'direct_file', expiresAt: 'Apr 15 08:00:00 2027 GMT', notAfterUnixSeconds: BASE_SAMPLED_AT_MS / 1000 + 6 * 86_400 },
    { name: 'internal', source: 'acme_managed', expiresAt: 'Oct 1 00:00:00 2027 GMT', notAfterUnixSeconds: BASE_SAMPLED_AT_MS / 1000 + 300 * 86_400 },
  ]
  wire.generation.retired = [
    { revision: 'rev-3', state: 'draining', ageSeconds: 90, deadlineRemainingSeconds: 30, inFlight: 5, inFlightByKind: { http: 5 } },
  ]
  wire.warnings.push({ code: 'W_SPARE', message: 'Pool files has a single server.' })
  wire.generation.activeRevision = '3f0c9e1ab27d46c58e90b3d1f7a2c64e8b5d0a9172ce4f3b6a81d05c9e7f2b4d' // a full digest
  edit(wire)
  return wire
}

/** Answers every snapshot request with the next document (the last one repeats); returns the number of requests. */
export async function serve(page: Page, documents: StatsDocument[]): Promise<() => number> {
  let served = 0
  await page.route('**/stats.html/snapshot.json', (route) => {
    const wire = documents[Math.min(served, documents.length - 1)]!
    served += 1
    return route.fulfill({ contentType: 'application/json', headers: { 'Cache-Control': 'no-store' }, body: JSON.stringify(wire) })
  })
  return () => served
}
