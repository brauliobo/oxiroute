import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import ConfigurationWorkspace from './ConfigurationWorkspace.vue'
import { emptyConfigSnapshot, jsonResponse } from './test/contractFixtures'

const token = 'configuration-memory-token'

describe('configuration event-driven revision checks', () => {
  let wrapper: VueWrapper | undefined
  let stream: ControllableStream | undefined

  afterEach(() => {
    wrapper?.unmount()
    stream?.close()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('refreshes a clean draft when a revision-bearing event arrives', async () => {
    const initial = emptyConfigSnapshot()
    const updated = updatedSnapshot(initial)
    stream = controllableStream('event: ready\ndata: {"cursor":0}\n\n')
    const fetch = installFetch(initial, updated, stream)

    wrapper = mount(ConfigurationWorkspace)
    await unlock(wrapper)
    stream.send(revisionEvent(updated.diskRevision))
    await flushPromises()

    expect(fetch.mock.calls.filter(([url]) => String(url) === '/api/v1/config')).toHaveLength(2)
    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('2')
    expect(wrapper.get('.revision-board').text()).toContain('disk-new')
    expect(wrapper.find('.revision-banner.stale').exists()).toBe(false)
  })

  it('preserves a dirty draft and marks it stale instead of applying the event snapshot', async () => {
    const initial = emptyConfigSnapshot()
    const updated = updatedSnapshot(initial)
    stream = controllableStream('event: ready\ndata: {"cursor":0}\n\n')
    installFetch(initial, updated, stream)

    wrapper = mount(ConfigurationWorkspace)
    await unlock(wrapper)
    await wrapper.get('[data-field="version"] input').setValue(3)
    stream.send(revisionEvent(updated.diskRevision))
    await flushPromises()

    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('3')
    expect(wrapper.get('.revision-banner.stale').text()).toContain('Draft preserved')
    expect(wrapper.get('.revision-banner.stale').text()).toContain('disk-new')
  })
})

function installFetch(
  initial: ReturnType<typeof emptyConfigSnapshot>,
  updated: ReturnType<typeof emptyConfigSnapshot>,
  stream: ControllableStream,
) {
  let configRequests = 0
  const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/config' && !init?.method) {
      configRequests += 1
      return Promise.resolve(jsonResponse(configRequests === 1 ? initial : updated))
    }
    if (url === '/api/v2/events/stream') return Promise.resolve(stream.response)
    throw new Error(`Unexpected request: ${url}`)
  })
  vi.stubGlobal('fetch', fetch)
  return fetch
}

async function unlock(wrapper: VueWrapper): Promise<void> {
  await wrapper.get('#config-access-token').setValue(token)
  await wrapper.get('form[data-unlock-form]').trigger('submit')
  await flushPromises()
}

function updatedSnapshot(snapshot: ReturnType<typeof emptyConfigSnapshot>) {
  return {
    ...structuredClone(snapshot),
    diskRevision: 'disk-new',
    candidateRevision: 'candidate-new',
    config: { ...structuredClone(snapshot.config), version: 2 },
  }
}

function revisionEvent(revision: string): string {
  return [
    'id: 1',
    'event: generation_activate',
    `data: {"cursor":1,"timestampUnixMs":null,"event":"generation_activate","outcome":"activated","revision":"${revision}"}`,
    '',
    '',
  ].join('\n')
}

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
      try {
        streamController?.close()
      } catch {
        // The component may already have cancelled the stream during teardown.
      }
    },
  }
}
