import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import App from '../App.vue'
import { contractMonitoring, jsonResponse } from '../test/contractFixtures'
import { flushPromises, mount } from '../test/mount'
import { documentTitle, NAV_ITEMS, viewFromHash, viewTitle } from './views'

const indexHtml = readFileSync(join(import.meta.dirname, '../../index.html'), 'utf8')

beforeEach(() => {
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse(contractMonitoring()))))
})

afterEach(() => {
  window.location.hash = ''
  localStorage.clear()
  document.documentElement.removeAttribute('data-theme')
  vi.unstubAllGlobals()
})

const navigate = async (wrapper: ReturnType<typeof mount>, hash: string) => {
  const changed = new Promise((resolve) => window.addEventListener('hashchange', resolve, { once: true }))
  await wrapper.get(`a[href="${hash}"]`).trigger('click')
  await changed
  await flushPromises()
}

describe('views', () => {
  it('maps every hash to its view and falls back to the overview', () => {
    expect(NAV_ITEMS.map(({ href }) => viewFromHash(href))).toEqual(NAV_ITEMS.map(({ id }) => id))
    expect(viewFromHash('')).toBe('overview')
    expect(viewFromHash('#/unknown')).toBe('overview')
  })

  it('titles the document by view', () => {
    expect(documentTitle('operations')).toBe('Operations - OxiRoute')
    expect(new Set(NAV_ITEMS.map(({ id }) => documentTitle(id))).size).toBe(NAV_ITEMS.length)
    expect(viewTitle('configuration')).toBe('Canonical configuration')
  })
})

describe('console shell', () => {
  it('has a skip link to the one main landmark, a header, the navigation and a footer', async () => {
    const wrapper = mount(App)
    await flushPromises()
    expect(wrapper.get('a.ox-skip').attributes('href')).toBe('#ox-main')
    expect(wrapper.findAll('main')).toHaveLength(1)
    expect(wrapper.get('main').attributes('id')).toBe('ox-main')
    expect(wrapper.findAll('h1')).toHaveLength(1)
    expect(wrapper.get('header h1').text()).toBe('OxiRoute')
    expect(wrapper.get('nav').attributes('aria-label')).toBe('Primary navigation')
    expect(wrapper.findAll('nav a')).toHaveLength(NAV_ITEMS.length)
    expect(wrapper.get('footer').text()).toContain('OxiRoute')
  })

  it('updates the document title and moves focus to the content on navigation', async () => {
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    expect(document.title).toBe('Runtime observatory - OxiRoute')
    await navigate(wrapper, '#/operations')
    expect(document.title).toBe('Operations - OxiRoute')
    expect(document.activeElement).toBe(wrapper.get('main').element)
    await navigate(wrapper, '#/audit')
    expect(document.title).toBe('Durable audit history - OxiRoute')
  })

  it('applies and remembers the chosen theme', async () => {
    const wrapper = mount(App)
    await flushPromises()
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false)
    await wrapper.get('header select').setValue('light')
    expect(document.documentElement.dataset.theme).toBe('light')
    expect(localStorage.getItem('oxiroute.theme')).toBe('light')
    await wrapper.get('header select').setValue('auto')
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false)
  })
})

describe('console document', () => {
  it('is comfortable, follows the system scheme and sets one theme colour per scheme', () => {
    expect(indexHtml).toContain('data-density="comfortable"')
    expect(indexHtml).toContain('<meta name="color-scheme" content="light dark" />')
    const colors = [...indexHtml.matchAll(/theme-color" media="\(prefers-color-scheme: (\w+)\)" content="(#\w+)"/g)]
    expect(colors.map(([, scheme]) => scheme)).toEqual(['light', 'dark'])
    const tokens = readFileSync(join(import.meta.dirname, '../design/tokens/color.css'), 'utf8')
    const [, light, dark] = tokens.match(/--ox-bg: light-dark\((#\w+), (#\w+)\)/)!
    expect(colors.map(([, , color]) => color)).toEqual([light, dark])
  })
})
