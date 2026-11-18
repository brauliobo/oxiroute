import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  ApiError,
  drainGeneration,
  fetchGenerations,
  fetchInflightRequests,
  fetchRegistrations,
  fetchStatus,
  registryDisabled,
  reloadGeneration,
  setServerWeight,
} from './api'
import { managementGeneration, managementStatus } from './test/managementFixtures'
import { inflightList, registrationList } from './test/operationsFixtures'

afterEach(() => {
  vi.unstubAllGlobals()
})

function stubFetch(body: unknown, status = 200): ReturnType<typeof vi.fn> {
  const fetch = vi.fn(() => Promise.resolve(Response.json(body, { status })))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

function sent(fetch: ReturnType<typeof vi.fn>): [string, RequestInit] {
  return fetch.mock.calls[0] as unknown as [string, RequestInit]
}

describe('server weight', () => {
  it('puts the targets, the weight and the expected revision', async () => {
    const fetch = stubFetch({ outcome: 'applied', changed: 1 })
    const targets = [{ pool: 'web', server: 'a' }]

    await expect(setServerWeight(targets, 40, 'rev', 'token')).resolves.toEqual({ outcome: 'applied', changed: 1 })

    const [url, init] = sent(fetch)
    expect(url).toBe('/api/v1/servers/weight')
    expect(init.method).toBe('PUT')
    expect(JSON.parse(String(init.body))).toEqual({ targets, weight: 40, expectedActiveRevision: 'rev' })
  })

  it('sends null to reset the weight', async () => {
    const fetch = stubFetch({ outcome: 'applied', changed: 1 })
    await setServerWeight([{ pool: 'web', server: 'a' }], null, 'rev', 'token')
    expect(JSON.parse(String(sent(fetch)[1].body)).weight).toBeNull()
  })

  it('rejects a malformed mutation response', async () => {
    stubFetch({ outcome: 'applied', changed: -1 })
    await expect(setServerWeight([{ pool: 'web', server: 'a' }], 2, 'rev', 'token')).rejects.toThrow('invalid response')
  })
})

describe('mutation responses', () => {
  it('keeps the drain reference count and the restart indicator of a reload', async () => {
    const restartRequired = { required: false, candidateRevision: null, since: null, reasons: [] }
    stubFetch({ outcome: 'startup_requested', candidateRevision: 'next', restartRequired })
    await expect(reloadGeneration('rev', 'token')).resolves.toEqual({
      outcome: 'startup_requested', candidateRevision: 'next', restartRequired,
    })

    stubFetch({ outcome: 'draining', activeReferences: 4 })
    await expect(drainGeneration('rev', 'token')).resolves.toEqual({ outcome: 'draining', activeReferences: 4 })
  })

  it('rejects a response with a malformed restart indicator', async () => {
    stubFetch({ outcome: 'startup_requested', candidateRevision: 'next', restartRequired: { required: 'yes' } })
    await expect(reloadGeneration('rev', 'token')).rejects.toThrow('invalid response')
  })
})

describe('restart indicator', () => {
  it('is decoded from the status and generation payloads', async () => {
    const reasons = [{ code: 'listener_ipv6_only', path: '/listeners/0', summary: 'IPv6-only needs a restart' }]
    const status = { ...managementStatus(), restartRequired: { required: true, candidateRevision: 'c', since: 5, reasons } }
    stubFetch(status)
    await expect(fetchStatus('token')).resolves.toMatchObject({ restartRequired: { required: true, reasons } })

    const generation = managementGeneration()
    generation.generation.restartRequired = status.restartRequired
    stubFetch(generation)
    await expect(fetchGenerations('token')).resolves.toMatchObject({ generation: { restartRequired: { required: true } } })
  })

  it('rejects payloads without a valid indicator', async () => {
    const { restartRequired: _omitted, ...status } = managementStatus()
    stubFetch(status)
    await expect(fetchStatus('token')).rejects.toThrow('invalid response')

    stubFetch({ ...managementStatus(), restartRequired: { required: true, candidateRevision: null, since: null, reasons: [{}] } })
    await expect(fetchStatus('token')).rejects.toThrow('invalid response')

    const generation = managementGeneration()
    generation.generation.pendingApply = { revision: 1 } as never
    stubFetch(generation)
    await expect(fetchGenerations('token')).rejects.toThrow('invalid response')
  })
})

describe('in-flight requests', () => {
  it('lists them without a query by default', async () => {
    const fetch = stubFetch(inflightList())
    await expect(fetchInflightRequests('token')).resolves.toEqual(inflightList())
    expect(sent(fetch)[0]).toBe('/api/v1/requests')
  })

  it('sends the filters the API defines', async () => {
    const fetch = stubFetch(inflightList())
    await fetchInflightRequests('token', {
      pool: 'web', route: 'api', phase: 'awaiting_headers', minAgeMs: 30_000, limit: 50, requestId: 'rq-1',
    })
    expect(sent(fetch)[0]).toBe(
      '/api/v1/requests?pool=web&route=api&phase=awaiting_headers&min_age_ms=30000&limit=50&requestId=rq-1',
    )
  })

  it.each([
    ['a non-decimal age', (list: ReturnType<typeof inflightList>) => { list.requests[0]!.ageMs = '-1' }],
    ['an unknown phase', (list: ReturnType<typeof inflightList>) => { (list.requests[0] as { phase: string }).phase = 'sleeping' }],
    ['a missing method', (list: ReturnType<typeof inflightList>) => { delete (list.requests[0] as { method?: string }).method }],
    ['a numeric total', (list: ReturnType<typeof inflightList>) => { (list as { total: unknown }).total = 3 }],
  ])('rejects %s', async (_name, break_) => {
    const list = inflightList()
    break_(list)
    stubFetch(list)
    await expect(fetchInflightRequests('token')).rejects.toThrow('invalid response')
  })

  it('surfaces a rejected query as an API error', async () => {
    stubFetch({ error: { code: 'invalid_request', message: 'unknown phase' } }, 400)
    await expect(fetchInflightRequests('token', { limit: 5000 })).rejects.toBeInstanceOf(ApiError)
  })
})

describe('registrations', () => {
  it('decodes the registry listing', async () => {
    const fetch = stubFetch(registrationList())
    await expect(fetchRegistrations('token')).resolves.toEqual(registrationList())
    expect(sent(fetch)[0]).toBe('/api/v1/registrations')
  })

  it.each([
    ['an unknown status', (list: ReturnType<typeof registrationList>) => { (list.registrations[0] as { status: string }).status = 'gone' }],
    ['a non-string address', (list: ReturnType<typeof registrationList>) => { (list.registrations[0]!.registration as { address: unknown }).address = 1 }],
    ['a label that is not text', (list: ReturnType<typeof registrationList>) => { list.registrations[0]!.registration.labels = { zone: 1 } as never }],
    ['a missing revision', (list: ReturnType<typeof registrationList>) => { delete (list as { registryRevision?: string }).registryRevision }],
  ])('rejects %s', async (_name, break_) => {
    const list = registrationList()
    break_(list)
    stubFetch(list)
    await expect(fetchRegistrations('token')).rejects.toThrow('invalid response')
  })

  it('recognises a daemon without a registry', async () => {
    stubFetch({ error: { code: 'registry_disabled', message: 'server registration is not configured' } }, 404)
    const error = await fetchRegistrations('token').catch((caught: unknown) => caught)
    expect(registryDisabled(error)).toBe(true)
    expect(registryDisabled(new ApiError(404, 'x', { error: { code: 'other', message: 'x' } }))).toBe(false)
  })
})
