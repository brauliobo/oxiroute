import { flushPromises, mount } from './test/mount'
import { afterEach, describe, expect, it, vi } from 'vitest'

import AuditWorkspace from './AuditWorkspace.vue'
import { jsonResponse } from './test/contractFixtures'
import { durableAuditPage, durableAuditStatus } from './test/managementFixtures'

const token = 'audit-test-token'

afterEach(() => {
  vi.unstubAllGlobals()
})

function degradedStatus(lastError?: string) {
  const status = durableAuditStatus()
  status.audit.state = 'degraded'
  status.audit.degraded = true
  if (lastError) status.audit.lastError = lastError
  return status
}

function stubAudit(status: unknown, page: unknown = durableAuditPage()) {
  const fetch = vi.fn((input: RequestInfo | URL) => {
    const url = String(input)
    if (url === '/api/v1/audit/status') return Promise.resolve(jsonResponse(status))
    if (url.startsWith('/api/v1/audit?')) return Promise.resolve(jsonResponse(page))
    throw new Error(`Unexpected request: ${url}`)
  })
  vi.stubGlobal('fetch', fetch)
  return fetch
}

describe('durable audit workspace', () => {
  it('loads fixed redacted fields, applies filters, and paginates by cursor', async () => {
    const first = durableAuditPage()
    first.hasMore = true
    first.latestCursor = 13
    const next = durableAuditPage()
    next.records[0]!.id = 13
    next.cursor = 13
    next.latestCursor = 13
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/audit/status') return Promise.resolve(jsonResponse(durableAuditStatus()))
      if (url.includes('/api/v1/audit?after=12')) return Promise.resolve(jsonResponse(next))
      return Promise.resolve(jsonResponse(first))
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('tbody tr').text()).toContain('server_update')
    expect(wrapper.get('tbody .ox-badge').text()).toBe('Succeeded')
    expect(wrapper.text()).toContain('Persistent')
    expect(wrapper.text()).toContain('1 record loaded')
    expect(wrapper.text()).not.toContain('Management token required')
    await wrapper.get('#audit-category').setValue('control')
    await wrapper.get('#audit-result').setValue('succeeded')
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/audit?after=0&limit=100&category=control&result=succeeded',
      expect.objectContaining({ headers: { Authorization: `Bearer ${token}` } }),
    )

    await wrapper.findAll('button').find((button) => button.text() === 'Load more durable records')!.trigger('click')
    await flushPromises()
    expect(wrapper.findAll('tbody tr').map((row) => row.get('th').text())).toEqual(['12', '13'])
  })

  it('clears the filters and queries the durable route again without them', async () => {
    const fetch = stubAudit(durableAuditStatus())
    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()
    const clear = () => wrapper.findAll('button').find((button) => button.text() === 'Clear filters')!
    expect(clear().attributes('disabled')).toBeDefined()

    await wrapper.get('#audit-result').setValue('failed')
    await wrapper.get('form').trigger('submit')
    await flushPromises()
    expect(clear().attributes('disabled')).toBeUndefined()
    await clear().trigger('click')
    await flushPromises()

    expect(String(fetch.mock.calls.at(-1)![0])).toBe('/api/v1/audit?after=0&limit=100')
    expect((wrapper.get('#audit-result').element as HTMLSelectElement).value).toBe('')
  })

  it('explains the persistence reason and what to check when the store is degraded', async () => {
    const page = durableAuditPage()
    page.records = []
    page.cursor = 0
    page.oldestCursor = null
    const fetch = stubAudit(degradedStatus('root_open_failed'), page)

    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    const banner = wrapper.get('.ox-banner').text()
    expect(banner).toContain('Durable audit is degraded.')
    expect(banner).toContain('root_open_failed')
    expect(banner).toContain('real directories the daemon may create and open')
    expect(wrapper.get('[role="status"] .ox-badge').text()).toBe('Degraded')
    expect(wrapper.text()).toContain('No durable audit records match this query.')
    expect(fetch.mock.calls.some(([input]) => String(input).includes('/api/v2/events'))).toBe(false)
  })

  it('gives generic guidance for a reason it does not know', async () => {
    stubAudit(degradedStatus('audit_store_write_failed'))
    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.ox-banner').text()).toContain('audit_store_write_failed')
    expect(wrapper.get('.ox-banner').text()).toContain('then the daemon log')
  })

  it('says when persistence is memory only', async () => {
    const status = durableAuditStatus()
    status.audit.state = 'memory'
    status.audit.persistent = false
    stubAudit(status)
    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.ox-banner').text()).toContain('Durable persistence is not active.')
    expect(wrapper.text()).toContain('Memory only')
  })

  it('relocks on unauthorized durable responses', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'unauthorized', message: 'token rejected' },
    }, 401))))

    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.emitted('unauthorized')).toBeTruthy()
  })

  it('rejects malformed records and disables the workspace when the route is unavailable', async () => {
    stubAudit(durableAuditStatus(), { records: [{}] })

    const malformed = mount(AuditWorkspace, { props: { token } })
    await flushPromises()
    expect(malformed.get('[role="alert"]').text()).toContain('invalid response payload')
    malformed.unmount()

    vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL) => {
      if (String(input).startsWith('/api/v1/audit/status')) return Promise.resolve(jsonResponse(durableAuditStatus()))
      return Promise.resolve(jsonResponse({ error: { code: 'route_not_found', message: 'route does not exist' } }, 404))
    }))
    const unavailable = mount(AuditWorkspace, { props: { token } })
    await flushPromises()
    expect(unavailable.text()).toContain('Durable audit unavailable.')
    expect(unavailable.text()).toContain('The non-durable Events view is not used as a fallback.')
    const refresh = unavailable.findAll('button').find((button) => button.text() === 'Refresh audit')!
    expect(refresh.attributes()).toHaveProperty('disabled')
    expect(unavailable.findAll('button').some((button) => button.text() === 'Retry audit capability check')).toBe(true)
  })

  it('counts one record in the singular', async () => {
    stubAudit(durableAuditStatus())
    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.text()).toContain('1 record loaded')
    expect(wrapper.text()).not.toContain('1 records')
  })
})
