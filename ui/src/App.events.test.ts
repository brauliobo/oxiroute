import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import { contractMonitoring, jsonResponse } from './test/contractFixtures'

describe('application event-driven refresh', () => {
  let wrapper: VueWrapper | undefined
  let stream: ControllableStream | undefined

  afterEach(() => {
    wrapper?.unmount()
    window.location.hash = ''
    stream?.close()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('refreshes monitoring when an operational event arrives after token unlock', async () => {
    window.location.hash = '#/stats'
    stream = controllableStream('event: ready\ndata: {"cursor":0}\n\n')
    let monitoringRequests = 0
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') {
        monitoringRequests += 1
        return Promise.resolve(jsonResponse(contractMonitoring()))
      }
      if (url === '/api/v1/events/stream') return Promise.resolve(stream!.response)
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    wrapper = mount(App)
    await flushPromises()
    expect(monitoringRequests).toBe(1)

    await wrapper.get('#management-access-token').setValue('in-memory-token')
    await wrapper.get('.management-auth').trigger('submit')
    await flushPromises()
    expect(monitoringRequests).toBe(2)

    stream.send([
      'id: 1',
      'event: generation_activate',
      'data: {"cursor":1,"timestampUnixMs":null,"event":"generation_activate",',
      'data: "outcome":"activated","revision":"revision-1"}',
      '',
      '',
    ].join('\n'))
    await flushPromises()

    expect(monitoringRequests).toBe(3)
    expect(fetch.mock.calls.some(([url]) => String(url) === '/api/v1/events/stream')).toBe(true)
  })
})

interface ControllableStream {
  response: Response
  send: (body: string) => void
  close: () => void
}

function controllableStream(initial: string): ControllableStream {
  let streamController: ReadableStreamDefaultController<Uint8Array> | undefined
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      streamController = controller
      controller.enqueue(new TextEncoder().encode(initial))
    },
  })
  return {
    response: new Response(body, {
      headers: { 'Content-Type': 'text/event-stream; charset=utf-8' },
    }),
    send(value) {
      streamController?.enqueue(new TextEncoder().encode(value))
    },
    close() {
      streamController?.close()
    },
  }
}
