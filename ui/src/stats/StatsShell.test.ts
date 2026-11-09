import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import '../test/dialog'
import { flushPromises, mount } from '../test/mount'
import { wireDocument } from './fixtures'
import type { StatsDocument } from './generated/statsPage'
import StatsShell from './StatsShell.vue'

const respond = (body: unknown, status = 200) => vi.fn<typeof fetch>(async () => new Response(JSON.stringify(body), { status }))
const page = async (document_: Partial<StatsDocument> = {}, url = '/haproxy') => {
  history.replaceState(null, '', url)
  const fetcher = respond(wireDocument(document_))
  vi.stubGlobal('fetch', fetcher)
  const wrapper = mount(StatsShell, { attachTo: document.body })
  await flushPromises()
  return { wrapper, fetcher }
}
const key = (name: string, target: Element = document.body, init: KeyboardEventInit = {}) =>
  target.dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true, ...init }))
const button = (wrapper: ReturnType<typeof mount>, text: string) =>
  wrapper.findAll('button').find((candidate) => candidate.text() === text)!

beforeEach(() => { history.replaceState(null, '', '/') })
afterEach(() => {
  vi.unstubAllGlobals()
  document.documentElement.removeAttribute('data-theme')
})

describe('StatsShell structure', () => {
  it('has the page landmarks, one h1 and a skip link to the main content', async () => {
    const { wrapper } = await page()
    expect(wrapper.findAll('h1')).toHaveLength(1)
    expect(wrapper.get('header').text()).toContain('OxiRoute statistics')
    expect(wrapper.get('[role=search]').attributes('aria-label')).toBe('Filter and view')
    expect(wrapper.get('main').attributes('id')).toBe('ox-main')
    expect(wrapper.get('main').attributes('tabindex')).toBe('-1')
    expect(wrapper.get('.ox-skip').attributes('href')).toBe('#ox-main')
    expect(wrapper.find('footer').exists()).toBe(true)
  })

  it('provides the store to its children, which show the live state and the export links of this page', async () => {
    const { wrapper, fetcher } = await page({}, '/haproxy/')
    expect(fetcher.mock.calls[0]![0]).toBe('/haproxy/snapshot.json')
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('live')
    expect(wrapper.get('a[href="/haproxy/stats.csv"]').text()).toBe('CSV')
    expect(wrapper.get('a[href="/haproxy/snapshot.json"]').text()).toBe('JSON')
    expect(wrapper.get('footer').text()).toContain('OxiRoute 6.3.0')
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

describe('StatsShell banners', () => {
  it('says that statistics are unavailable instead of showing numbers while nothing was loaded', async () => {
    history.replaceState(null, '', '/haproxy')
    vi.stubGlobal('fetch', respond({ error: { code: 'stats_unavailable', message: 'x' } }, 503))
    const wrapper = mount(StatsShell)
    await flushPromises()
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('error')
    expect(wrapper.get('[role=alert]').text()).toContain('Statistics are unavailable (the server cannot sample statistics right now).')
    expect(wrapper.find('footer').text()).not.toContain('generation')
  })

  it('asks for a reload when the server speaks a newer format', async () => {
    vi.stubGlobal('fetch', respond({ ...wireDocument(), schemaVersion: 2 }))
    const wrapper = mount(StatsShell)
    await flushPromises()
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('unsupported')
    expect(wrapper.get('[role=alert]').text()).toContain('Reload the page')
    expect(button(wrapper, 'Reload page').exists()).toBe(true)
  })

  it('retries on request from the banner', async () => {
    const fetcher = respond({ error: { code: 'stats_unavailable', message: 'x' } }, 503)
    vi.stubGlobal('fetch', fetcher)
    const wrapper = mount(StatsShell)
    await flushPromises()
    await button(wrapper, 'Retry now').trigger('click')
    await flushPromises()
    expect(fetcher).toHaveBeenCalledTimes(2)
  })
})

describe('StatsShell view state', () => {
  it('restores the view from the URL into the controls', async () => {
    const { wrapper } = await page({}, '/haproxy?q=origin&state=down&problems=1&sort=-rate&interval=5000&paused=1&theme=dark')
    expect((wrapper.get('#ox-search').element as HTMLInputElement).value).toBe('origin')
    expect(button(wrapper, 'DOWN').attributes('aria-pressed')).toBe('true')
    expect(button(wrapper, 'Problems only').attributes('aria-pressed')).toBe('true')
    expect((wrapper.get('select[name=sort]').element as HTMLSelectElement).value).toBe('rate')
    expect(button(wrapper, 'Descending').attributes('aria-pressed')).toBe('true')
    expect((wrapper.get('select[name=interval]').element as HTMLSelectElement).value).toBe('5000')
    expect(button(wrapper, 'Resume').exists()).toBe(true)
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('paused')
    expect(document.documentElement.dataset.theme).toBe('dark')
  })

  it('writes each control into the address bar', async () => {
    const { wrapper } = await page()
    await wrapper.get('#ox-search').setValue('api')
    expect(location.search).toBe('?q=api')
    await button(wrapper, 'UP').trigger('click')
    await wrapper.get('select[name=sort]').setValue('status')
    await wrapper.get('select[name=interval]').setValue('10000')
    await wrapper.get('select[name=theme]').setValue('light')
    expect(location.search).toBe('?q=api&state=up&sort=status&interval=10000&theme=light')
    expect(document.documentElement.dataset.theme).toBe('light')
    await wrapper.get('select[name=theme]').setValue('auto')
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false)
  })

  it('pauses and resumes polling from the button', async () => {
    const { wrapper, fetcher } = await page()
    await button(wrapper, 'Pause').trigger('click')
    expect(wrapper.get('[role=status]').attributes('data-status')).toBe('paused')
    expect(location.search).toBe('?paused=1')
    await button(wrapper, 'Refresh now').trigger('click')
    await flushPromises()
    expect(fetcher).toHaveBeenCalledTimes(2)
    await button(wrapper, 'Resume').trigger('click')
    expect(location.search).toBe('')
  })
})

describe('StatsShell shortcuts', () => {
  it('"/" focuses the search box and does not type into it', async () => {
    const { wrapper } = await page()
    const event = new KeyboardEvent('keydown', { key: '/', bubbles: true, cancelable: true })
    document.body.dispatchEvent(event)
    expect(event.defaultPrevented).toBe(true)
    expect(document.activeElement).toBe(wrapper.get('#ox-search').element)
    expect(key('p', document.activeElement!)).toBe(true)
    expect(location.search).toBe('')
  })

  it('p pauses, r refreshes, t cycles the theme and f toggles problems only', async () => {
    const { wrapper, fetcher } = await page()
    key('p')
    await flushPromises()
    expect(location.search).toBe('?paused=1')
    key('r')
    await flushPromises()
    expect(fetcher).toHaveBeenCalledTimes(2)
    key('t')
    key('f')
    await flushPromises()
    expect(location.search).toBe('?problems=1&paused=1&theme=light')
    expect(document.documentElement.dataset.theme).toBe('light')
    wrapper.unmount()
  })

  it('? opens the shortcut help, and the shortcuts do not act behind it', async () => {
    const { wrapper } = await page()
    key('?', document.body, { shiftKey: true })
    await flushPromises()
    const dialog = wrapper.get('dialog')
    expect(dialog.attributes('open')).toBeDefined()
    key('p', dialog.get('button').element)
    expect(location.search).toBe('')
    await dialog.get('button').trigger('click')
    expect(dialog.attributes('open')).toBeUndefined()
  })

  it('stops listening to the keyboard when the app unmounts', async () => {
    const { wrapper } = await page()
    wrapper.unmount()
    key('p')
    expect(location.search).toBe('')
  })
})

describe('StatsShell administration', () => {
  it('shows nothing of it unless the server says admin', async () => {
    const { wrapper } = await page({ admin: false })
    expect(wrapper.find('section[aria-labelledby]').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('Administration')
    expect(button(wrapper, 'Drain')?.exists()).toBeFalsy()
  })

  it('offers it with the active revision when it does, and refreshes after a change', async () => {
    const responses = [
      new Response(JSON.stringify(wireDocument({ admin: true })), { status: 200 }),
      new Response(null, { status: 204 }),
      new Response(JSON.stringify(wireDocument({ admin: true })), { status: 200 }),
    ]
    const fetcher = vi.fn<typeof fetch>(async () => responses.shift()!)
    history.replaceState(null, '', '/haproxy')
    vi.stubGlobal('fetch', fetcher)
    const wrapper = mount(StatsShell, { attachTo: document.body })
    await flushPromises()
    expect(wrapper.text()).toContain('Administration')
    await button(wrapper, 'Drain').trigger('click')
    await button(wrapper, 'Confirm').trigger('click')
    await flushPromises()
    const [url, init] = fetcher.mock.calls[1]!
    expect([url, init!.method]).toEqual(['/haproxy', 'POST'])
    expect((init!.body as URLSearchParams).get('generation_revision')).toBe('rev-4')
    expect(fetcher.mock.calls[2]![0]).toBe('/haproxy/snapshot.json')
  })

  it('offers nothing without an active revision to post', async () => {
    const wire = wireDocument({ admin: true })
    delete wire.generation.activeRevision
    const { wrapper } = await page(wire)
    expect(wrapper.text()).not.toContain('Administration')
  })
})
