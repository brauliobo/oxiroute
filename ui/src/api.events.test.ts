import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  connectEventStream,
  parseEventStreamFrame,
  type EventStreamClient,
} from './api'

const eventStreamHeaders = { 'Content-Type': 'text/event-stream; charset=utf-8' }

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('operational event stream client', () => {
  it('uses the in-memory token in the bearer header without putting it in the URL', async () => {
    const token = 'private-management-token'
    const fetch = vi.fn(() => Promise.resolve(eventResponse('event: ready\ndata: {"cursor":4}\n\n')))
    vi.stubGlobal('fetch', fetch)
    let client!: EventStreamClient
    client = connectEventStream(token, { onReady: () => client.close() })

    await client.closed

    expect(fetch).toHaveBeenCalledWith('/api/v1/events/stream', expect.objectContaining({
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
