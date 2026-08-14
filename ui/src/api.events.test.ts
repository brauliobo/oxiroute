import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  connectEventStream,
  fetchEvents,
  parseEventStreamFrame,
  type EventStreamClient,
} from './api'

const eventStreamHeaders = { 'Content-Type': 'text/event-stream; charset=utf-8' }

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('operational event stream client', () => {
  it('uses the version-two event stream for the corrected contract', async () => {
    const fetch = vi.fn(() => Promise.resolve(eventResponse('event: ready\ndata: {"cursor":4}\n\n')))
    vi.stubGlobal('fetch', fetch)
    let client!: EventStreamClient
    client = connectEventStream('token', { onReady: () => client.close() })

    await client.closed

    const firstCall = fetch.mock.calls[0] as unknown as [RequestInfo | URL]
    expect(firstCall[0]).toBe('/api/v2/events/stream')
  })

  it('falls back to the shipped stream only when v2 is absent', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(null, { status: 404 }))
      .mockResolvedValueOnce(eventResponse('event: ready\ndata: {"cursor":4}\n\n'))
    vi.stubGlobal('fetch', fetch)
    let client!: EventStreamClient
    client = connectEventStream('token', { onReady: () => client.close() })

    await client.closed

    expect(fetch.mock.calls.map(([path]) => path)).toEqual([
      '/api/v2/events/stream',
      '/api/v1/events/stream',
    ])
  })

  it('does not hide a version-two stream authentication failure with v1 fallback', async () => {
    const fetch = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => Promise.resolve(new Response(
      JSON.stringify({ error: { code: 'unauthorized', message: 'authentication required' } }),
      { status: 401, headers: { 'Content-Type': 'application/json' } },
    )))
    const onError = vi.fn()
    vi.stubGlobal('fetch', fetch)

    const client = connectEventStream('wrong-token', { onError }, { maxRetries: 0 })
    await client.closed

    expect(fetch.mock.calls.map(([path]) => path)).toEqual(['/api/v2/events/stream'])
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({ status: 401 }))
  })

  it('does not hide a version-two page authentication failure with v1 fallback', async () => {
    const fetch = vi.fn((_input: RequestInfo | URL, _init?: RequestInit) => Promise.resolve(new Response(
      JSON.stringify({ error: { code: 'unauthorized', message: 'authentication required' } }),
      { status: 401, headers: { 'Content-Type': 'application/json' } },
    )))
    vi.stubGlobal('fetch', fetch)

    await expect(fetchEvents(0, 1, 'wrong-token')).rejects.toMatchObject({ status: 401 })
    expect(fetch.mock.calls.map(([path]) => path)).toEqual(['/api/v2/events?after=0&limit=1'])
  })

  it('uses the in-memory token in the bearer header without putting it in the URL', async () => {
    const token = 'private-management-token'
    const fetch = vi.fn(() => Promise.resolve(eventResponse('event: ready\ndata: {"cursor":4}\n\n')))
    vi.stubGlobal('fetch', fetch)
    let client!: EventStreamClient
    client = connectEventStream(token, { onReady: () => client.close() })

    await client.closed

    expect(fetch).toHaveBeenCalledWith('/api/v2/events/stream', expect.objectContaining({
      cache: 'no-store',
      headers: {
        Accept: 'text/event-stream',
        Authorization: `Bearer ${token}`,
        'Cache-Control': 'no-cache',
      },
    }))
    const firstCall = fetch.mock.calls[0] as unknown as [RequestInfo | URL]
    expect(String(firstCall[0])).not.toContain(token)
  })

  it('parses typed operational data and drops unsupported fields', () => {
    const message = parseEventStreamFrame([
      'id: 7',
      'event: generation_activate',
      'data: {"cursor":7,"timestampUnixMs":1234,"event":"generation_activate",',
      'data: "outcome":"activated","revision":"revision-7",',
      'data: "authorization":"private-secret"}',
      '',
    ].join('\n'))

    expect(message).toEqual({
      type: 'operational',
      event: {
        cursor: 7,
        timestampUnixMs: 1234,
        event: 'generation_activate',
        outcome: 'activated',
        revision: 'revision-7',
      },
    })
    expect(JSON.stringify(message)).not.toContain('private-secret')
    expect(parseEventStreamFrame('event: generation_activate\nid: 8\ndata: {"cursor":7}\n\n')).toBeNull()
  })

  it('keeps the certificate identity for redacted certificate job events', () => {
    const message = parseEventStreamFrame([
      'id: 12',
      'event: certificate_renewal',
      'data: {"cursor":12,"timestampUnixMs":null,"event":"certificate_renewal",',
      'data: "outcome":"failed","revision":null,"certificate":"edge-example"}',
      '',
    ].join('\n'))

    expect(message).toEqual({
      type: 'operational',
      event: {
        cursor: 12,
        timestampUnixMs: null,
        event: 'certificate_renewal',
        outcome: 'failed',
        revision: null,
        certificate: 'edge-example',
      },
    })
  })

  it('normalizes the shipped v1 certificate activation mismatch', () => {
    expect(parseEventStreamFrame([
      'id: 13',
      'event: certificate_activated',
      'data: {"cursor":13,"timestampUnixMs":null,"event":"certificate_activation",',
      'data: "outcome":"activated","revision":null,"certificate":"edge-example"}',
      '',
    ].join('\n'))).toEqual({
      type: 'operational',
      event: {
        cursor: 13,
        timestampUnixMs: null,
        event: 'certificate_activated',
        outcome: 'activated',
        revision: null,
        certificate: 'edge-example',
      },
    })
  })

  it('accepts every Rust operational event name with a producer-defined outcome', () => {
    for (const [index, [event, outcomes]] of Object.entries(SIMPLE_EVENT_OUTCOMES).entries()) {
      const cursor = index + 1
      expect(parseEventStreamFrame(
        `id: ${cursor}\nevent: ${event}\ndata: {"cursor":${cursor},"timestampUnixMs":null,"event":"${event}","outcome":"${outcomes[0]}","revision":null}\n\n`,
      )).toMatchObject({ type: 'operational', event: { cursor, event } })
    }

    expect(operationalEvent('upstream_endpoint_ejection', EJECTED_OUTCOME)).not.toBeNull()
    expect(operationalEvent('upstream_endpoint_recovery', RECOVERED_OUTCOME)).not.toBeNull()
  })

  it('accepts only producer-defined simple outcomes for each operational event name', () => {
    for (const [event, accepted] of Object.entries(SIMPLE_EVENT_OUTCOMES)) {
      for (const outcome of SIMPLE_OUTCOMES) {
        const message = operationalEvent(event, JSON.stringify(outcome))
        if (accepted.includes(outcome)) expect(message).not.toBeNull()
        else expect(message, `${event} must reject ${outcome}`).toBeNull()
      }
    }
  })

  it('rejects impossible operational event name and outcome pairs', () => {
    expect(operationalEvent('control_operation', '"applied"')).toBeNull()
    expect(operationalEvent('server_update', EJECTED_OUTCOME)).toBeNull()
    expect(operationalEvent('upstream_endpoint_ejection', RECOVERED_OUTCOME)).toBeNull()
    expect(operationalEvent('upstream_endpoint_ejection', '"applied"')).toBeNull()
    expect(operationalEvent('upstream_endpoint_recovery', EJECTED_OUTCOME)).toBeNull()
    expect(operationalEvent('upstream_endpoint_recovery', '"applied"')).toBeNull()
  })

  it('rejects non-string health failure values in structured outcomes', () => {
    const event = (reason: string) => parseEventStreamFrame(
      `id: 31\nevent: upstream_endpoint_ejection\ndata: {"cursor":31,"timestampUnixMs":null,"event":"upstream_endpoint_ejection","outcome":{"type":"ejected","pool":"backend","server":"primary","reason":${reason},"failureCount":3,"ejectionCount":2,"ejectedAtUnixMs":1200,"ejectionUntilUnixMs":2200},"revision":null}\n\n`,
    )

    expect(event('["connect_failed"]')).toBeNull()
    expect(event('{"value":"connect_failed"}')).toBeNull()
  })

  it('projects structured endpoint ejection and recovery outcomes without unsupported fields', () => {
    const ejection = parseEventStreamFrame([
      'id: 21',
      'event: upstream_endpoint_ejection',
      'data: {"cursor":21,"timestampUnixMs":1234,"event":"upstream_endpoint_ejection",',
      'data: "outcome":{"type":"ejected","pool":"backend","server":"primary",',
      'data: "reason":"connect_failed","failureCount":3,"ejectionCount":2,',
      'data: "ejectedAtUnixMs":1200,"ejectionUntilUnixMs":2200,"authorization":"private-secret"},',
      'data: "revision":null}',
      '',
    ].join('\n'))
    const recovery = parseEventStreamFrame([
      'id: 22',
      'event: upstream_endpoint_recovery',
      'data: {"cursor":22,"timestampUnixMs":2234,"event":"upstream_endpoint_recovery",',
      'data: "outcome":{"type":"recovered","pool":"backend","server":"primary",',
      'data: "reason":null,"recoveryCount":1,"recoveredAtUnixMs":2234,"cookie":"session-secret"},',
      'data: "revision":null}',
      '',
    ].join('\n'))

    expect(ejection).toEqual({
      type: 'operational',
      event: {
        cursor: 21,
        timestampUnixMs: 1234,
        event: 'upstream_endpoint_ejection',
        outcome: {
          type: 'ejected',
          pool: 'backend',
          server: 'primary',
          reason: 'connect_failed',
          failureCount: 3,
          ejectionCount: 2,
          ejectedAtUnixMs: 1200,
          ejectionUntilUnixMs: 2200,
        },
        revision: null,
      },
    })
    expect(recovery).toEqual({
      type: 'operational',
      event: {
        cursor: 22,
        timestampUnixMs: 2234,
        event: 'upstream_endpoint_recovery',
        outcome: {
          type: 'recovered',
          pool: 'backend',
          server: 'primary',
          reason: null,
          recoveryCount: 1,
          recoveredAtUnixMs: 2234,
        },
        revision: null,
      },
    })
    expect(JSON.stringify([ejection, recovery])).not.toContain('private-secret')
    expect(JSON.stringify([ejection, recovery])).not.toContain('session-secret')
  })

  it('reconnects with the last event id after a bounded stream interruption', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(eventResponse([
        'event: ready',
        'data: {"cursor":2}',
        '',
        'id: 3',
        'event: generation_prepare',
        'data: {"cursor":3,"timestampUnixMs":null,"event":"generation_prepare",',
        'data: "outcome":"prepared","revision":null}',
        '',
        '',
      ].join('\n')))
      .mockImplementationOnce((_input: RequestInfo | URL, init?: RequestInit) => {
        expect(init?.headers).toEqual(expect.objectContaining({ 'Last-Event-ID': '3' }))
        return Promise.resolve(eventResponse([
          'event: ready',
          'data: {"cursor":3}',
          '',
          'id: 4',
          'event: generation_activate',
          'data: {"cursor":4,"timestampUnixMs":null,"event":"generation_activate",',
          'data: "outcome":"activated","revision":null}',
          '',
          '',
        ].join('\n')))
      })
    vi.stubGlobal('fetch', fetch)
    let client!: EventStreamClient
    client = connectEventStream('token', {
      onEvent: (event) => {
        if (event.cursor === 4) client.close()
      },
    }, { maxRetries: 1, retryDelayMs: 0 })

    await client.closed

    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('reloads through resync and reconnects from the latest cursor', async () => {
    expect(parseEventStreamFrame('event: resync_required\ndata: {"cursor":1,"oldestCursor":3,"latestCursor":9}\n\n')).toEqual({
      type: 'resync_required',
      data: { cursor: 1, oldestCursor: 3, latestCursor: 9 },
    })
    const fetch = vi.fn()
      .mockResolvedValueOnce(eventResponse([
        'event: resync_required',
        'data: {"cursor":1,"oldestCursor":3,"latestCursor":9}',
        '',
        '',
      ].join('\n')))
      .mockImplementationOnce((_input: RequestInfo | URL, init?: RequestInit) => {
        expect(init?.headers).toEqual(expect.objectContaining({ 'Last-Event-ID': '9' }))
        return Promise.resolve(eventResponse([
          'event: ready',
          'data: {"cursor":9}',
          '',
          'id: 10',
          'event: server_update',
          'data: {"cursor":10,"timestampUnixMs":null,"event":"server_update",',
          'data: "outcome":"applied","revision":null}',
          '',
          '',
        ].join('\n')))
      })
    vi.stubGlobal('fetch', fetch)
    const resync = vi.fn()
    const onError = vi.fn()
    let client!: EventStreamClient
    client = connectEventStream('token', {
      onResyncRequired: resync,
      onError,
      onEvent: (event) => {
        if (event.cursor === 10) client.close()
      },
    }, { maxRetries: 0 })

    await client.closed

    expect(onError).not.toHaveBeenCalled()
    expect(resync).toHaveBeenCalledWith({ cursor: 1, oldestCursor: 3, latestCursor: 9 })
    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('stops after the configured retry budget', async () => {
    const fetch = vi.fn(() => Promise.reject(new TypeError('network unavailable')))
    const onError = vi.fn()
    vi.stubGlobal('fetch', fetch)
    const client = connectEventStream('token', { onError }, {
      maxRetries: 2,
      retryDelayMs: 0,
    })

    await client.closed

    expect(fetch).toHaveBeenCalledTimes(3)
    expect(onError).toHaveBeenCalledTimes(3)
  })
})

function eventResponse(body: string): Response {
  return new Response(body, { headers: eventStreamHeaders })
}

const SIMPLE_OUTCOMES = [
  'prepared', 'rejected', 'activated', 'quarantined', 'requested', 'applied', 'failed', 'unknown',
] as const

const SIMPLE_EVENT_OUTCOMES: Record<string, readonly (typeof SIMPLE_OUTCOMES)[number][]> = {
  generation_prepare: ['prepared', 'rejected', 'requested', 'failed'],
  generation_activate: ['activated'],
  generation_rollback: ['prepared', 'rejected', 'requested', 'failed'],
  generation_drain: ['rejected', 'requested', 'failed'],
  generation_start: ['quarantined'],
  configuration_reload: ['rejected', 'applied', 'failed'],
  import_completed: ['applied'],
  control_operation: ['rejected', 'requested', 'failed'],
  process_shutdown: ['rejected', 'requested', 'failed'],
  listener_administrative_state: ['rejected', 'applied', 'failed'],
  pool_administrative_state: ['rejected', 'applied', 'failed'],
  server_update: ['rejected', 'applied', 'failed'],
  rtmp_connect: ['rejected', 'applied', 'failed'],
  rtmp_publish: ['rejected', 'applied', 'failed'],
  rtmp_play: ['rejected', 'applied', 'failed'],
  rtmp_disconnect: ['rejected', 'applied', 'failed'],
  rtmp_access: ['rejected', 'applied', 'failed'],
  certificate_renewal: ['rejected', 'requested', 'applied', 'failed'],
  certificate_activated: ['activated'],
  certificate_revocation: ['rejected', 'requested', 'applied', 'failed'],
  certificate_deletion: ['rejected', 'requested', 'applied', 'failed'],
  certificate_account_rollover: ['rejected', 'requested', 'applied', 'failed'],
  certificate_job_control: ['rejected', 'requested', 'applied', 'failed'],
  unknown: ['unknown'],
}

const EJECTED_OUTCOME = '{"type":"ejected","pool":"backend","server":"primary","reason":"connect_failed","failureCount":3,"ejectionCount":2,"ejectedAtUnixMs":1200,"ejectionUntilUnixMs":2200}'
const RECOVERED_OUTCOME = '{"type":"recovered","pool":"backend","server":"primary","reason":null,"recoveryCount":1,"recoveredAtUnixMs":2234}'

function operationalEvent(name: string, outcome: string): ReturnType<typeof parseEventStreamFrame> {
  return parseEventStreamFrame(
    `id: 30\nevent: ${name}\ndata: {"cursor":30,"timestampUnixMs":null,"event":"${name}","outcome":${outcome},"revision":null}\n\n`,
  )
}
