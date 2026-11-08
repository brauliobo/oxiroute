import { afterEach, describe, expect, it, vi } from 'vitest'

import { ApiError, evacuatePools, evacuateServers } from './api'

const report = {
  outcome: 'evacuated',
  force: false,
  cancelledAndRedispatched: 2,
  forced: 0,
  leftRunning: { no_destination: 1 },
}

afterEach(() => {
  vi.unstubAllGlobals()
})

function stubFetch(response: Response): ReturnType<typeof vi.fn> {
  const fetch = vi.fn(() => Promise.resolve(response))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

function sentBody(fetch: ReturnType<typeof vi.fn>): unknown {
  const [, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
  return JSON.parse(String(init.body))
}

describe('evacuation mutations', () => {
  it('posts the targets, the force flag and the expected revision', async () => {
    const fetch = stubFetch(Response.json(report))

    await expect(evacuateServers([{ pool: 'gpu-render', server: 'node-a' }], true, 'rev', 'token'))
      .resolves.toEqual(report)

    expect(fetch.mock.calls[0]?.[0]).toBe('/api/v1/servers/evacuate')
    expect(sentBody(fetch)).toEqual({
      targets: [{ pool: 'gpu-render', server: 'node-a' }],
      force: true,
      expectedActiveRevision: 'rev',
    })
  })

  it('evacuates whole pools', async () => {
    const fetch = stubFetch(Response.json(report))

    await evacuatePools(['gpu-render'], false, 'rev', 'token')

    expect(fetch.mock.calls[0]?.[0]).toBe('/api/v1/pools/evacuate')
    expect(sentBody(fetch)).toEqual({ pools: ['gpu-render'], force: false, expectedActiveRevision: 'rev' })
  })

  it('rejects a response that is not an evacuation report', async () => {
    stubFetch(Response.json({ ...report, cancelledAndRedispatched: -1 }))
    await expect(evacuatePools(['gpu-render'], false, 'rev', 'token')).rejects.toThrow()

    stubFetch(Response.json({ error: { code: 'pool_not_found', message: 'missing' } }, { status: 404 }))
    await expect(evacuatePools(['missing'], false, 'rev', 'token')).rejects.toBeInstanceOf(ApiError)
  })
})
