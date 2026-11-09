import { afterEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '../test/mount'
import { wireDocument } from './fixtures'
import StatsShell from './StatsShell.vue'

const respond = (body: unknown, status = 200) => vi.fn<typeof fetch>(async () => new Response(JSON.stringify(body), { status }))
afterEach(() => { vi.unstubAllGlobals() })

describe('StatsShell', () => {
  it('provides the store to its children, which show the live summary', async () => {
    const fetcher = respond(wireDocument())
    vi.stubGlobal('fetch', fetcher)
    const wrapper = mount(StatsShell)
    expect(wrapper.get('[role=status]').text()).toContain('Loading')
    await flushPromises()
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('live')
    expect(wrapper.text()).toContain('2 of 3 servers up')
    expect(fetcher.mock.calls[0]![0]).toBe('/snapshot.json')
  })

  it('shows that statistics are unavailable instead of numbers while nothing was loaded', async () => {
    vi.stubGlobal('fetch', respond({ error: { code: 'stats_unavailable', message: 'x' } }, 503))
    const wrapper = mount(StatsShell)
    await flushPromises()
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('error')
    expect(wrapper.find('.stats-servers').exists()).toBe(false)
  })

  it('stops polling when the app unmounts', async () => {
    const signals: AbortSignal[] = []
    vi.stubGlobal('fetch', vi.fn<typeof fetch>((_url, init) => {
      signals.push(init!.signal!)
      return new Promise<Response>(() => {})
    }))
    const wrapper = mount(StatsShell)
    expect(signals).toHaveLength(1)
    wrapper.unmount()
    expect(signals[0]!.aborted).toBe(true)
  })
})
