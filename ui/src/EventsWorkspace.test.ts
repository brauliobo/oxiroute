import { afterEach, describe, expect, it, vi } from 'vitest'
import EventsWorkspace from './EventsWorkspace.vue'
import { jsonResponse } from './test/contractFixtures'
import { flushPromises, mount } from './test/mount'

const token = 'events-test-token'
const encoder = new TextEncoder()

const event = (cursor: number, name = 'generation_activate', outcome = 'activated', extra = {}) =>
  ({ cursor, timestampUnixMs: 1_754_000_000_000 + cursor * 1000, event: name, outcome, revision: null, ...extra })

const page = (events: unknown[], latest = events.length, hasMore = false) =>
  ({ events, cursor: latest, latestCursor: latest, hasMore, oldestCursor: events.length ? 1 : null })

const shutdown = () => new Response('event: shutdown\ndata: {"reason":"server_shutdown"}\n\n', {
  headers: { 'Content-Type': 'text/event-stream' },
})

const openStream = (frame: string) => new Response(new ReadableStream({
  start: (controller) => controller.enqueue(encoder.encode(frame)),
}), { headers: { 'Content-Type': 'text/event-stream' } })

function stubApi(pages: unknown[], stream: () => Promise<Response> | Response = shutdown) {
  let served = 0
  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL) => {
    const url = String(input)
    if (url.startsWith('/api/v2/events?')) return Promise.resolve(jsonResponse(pages[Math.min(served++, pages.length - 1)]))
    if (url === '/api/v2/events/stream') return Promise.resolve(stream())
    throw new Error(`Unexpected request: ${url}`)
  }))
}

afterEach(() => vi.unstubAllGlobals())

describe('events workspace', () => {
  it('lists events in a table with word badges and counts one event in the singular', async () => {
    stubApi([page([event(1, 'generation_prepare', 'prepared')])])
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('h2').text()).toBe('Events')
    expect(wrapper.text()).toContain('1 event')
    expect(wrapper.text()).not.toContain('1 events')
    expect(wrapper.text()).not.toContain('Management token required')
    expect(wrapper.get('tbody').text()).toContain('Generation prepare')
    expect(wrapper.get('tbody .ox-badge').text()).toBe('Prepared')
  })

  it('filters by search text and by event name and says how many are shown', async () => {
    stubApi([page([
      event(1, 'generation_activate'),
      event(2, 'certificate_renewal', 'requested', { certificate: 'edge.example.com' }),
      event(3, 'server_update', 'failed'),
    ])])
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()
    expect(wrapper.findAll('tbody tr')).toHaveLength(3)

    await wrapper.get('#events-search').setValue('EDGE.example')
    expect(wrapper.findAll('tbody tr').map((row) => row.text())).toEqual([expect.stringContaining('Certificate renewal')])
    expect(wrapper.text()).toContain('Showing 1 of 3 events')

    await wrapper.get('#events-search').setValue('')
    await wrapper.get('#events-name').setValue('server_update')
    expect(wrapper.findAll('tbody tr')).toHaveLength(1)
    expect(wrapper.get('tbody').text()).toContain('Failed')

    await wrapper.get('#events-search').setValue('nothing like this')
    expect(wrapper.findAll('tbody tr')).toHaveLength(0)
    expect(wrapper.text()).toContain('No matching events')
  })

  it('offers only the event names that were retained', async () => {
    stubApi([page([event(1, 'server_update', 'failed'), event(2, 'server_update', 'applied')])])
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.findAll('#events-name option').map((option) => option.text())).toEqual(['All events', 'Server update'])
  })

  it('shows an empty ring honestly', async () => {
    stubApi([page([])])
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('No operational events have been retained.')
    expect(wrapper.find('table').exists()).toBe(false)
  })

  it('marks a restarted ring when a reload reports a lower latest cursor', async () => {
    stubApi([page([event(1), event(2), event(3), event(4), event(5), event(6), event(7)]), page([event(1), event(2)])])
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()
    expect(wrapper.text()).not.toContain('The event ring restarted.')

    await wrapper.findAll('button').find((button) => button.text() === 'Reload history')!.trigger('click')
    await flushPromises()

    expect(wrapper.text()).toContain('The event ring restarted. Earlier history was lost.')
    expect(wrapper.text()).toContain('latest cursor is now 2, it was 7')
    expect(wrapper.findAll('tbody tr')).toHaveLength(2)
  })

  it('marks a restarted ring when the live connection starts behind the retained cursor', async () => {
    stubApi([page([event(1), event(2), event(3)]), page([event(1)])], () => openStream('event: ready\ndata: {"cursor":1}\n\n'))
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('The event ring restarted.')
    expect(wrapper.findAll('tbody tr')).toHaveLength(1)
    wrapper.unmount()
  })

  it('says Live while the stream is connected and Offline after the server shuts it down', async () => {
    stubApi([page([event(1)])], () => openStream('event: ready\ndata: {"cursor":1}\n\n'))
    const live = mount(EventsWorkspace, { props: { token } })
    await flushPromises()
    expect(live.get('[data-state]').text()).toBe('Live')
    live.unmount()

    stubApi([page([event(1)])])
    const offline = mount(EventsWorkspace, { props: { token } })
    await flushPromises()
    expect(offline.get('[data-state]').text()).toBe('Offline')
    expect(offline.text()).toContain('The server closed live updates because it is shutting down.')
  })

  it('says Reconnecting when the live connection drops', async () => {
    stubApi([page([event(1)])], () => Promise.reject(new TypeError('network down')) as never)
    const wrapper = mount(EventsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('[data-state]').text()).toBe('Reconnecting')
    wrapper.unmount()
  })
})
