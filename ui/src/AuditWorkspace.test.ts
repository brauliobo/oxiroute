import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import AuditWorkspace from './AuditWorkspace.vue'
import { jsonResponse } from './test/contractFixtures'
import { durableAuditPage, durableAuditStatus } from './test/managementFixtures'

const token = 'audit-test-token'

afterEach(() => {
  vi.unstubAllGlobals()
})

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

    expect(wrapper.get('.audit-record').text()).toContain('server_update')
    expect(wrapper.get('.status-panel').text()).toContain('Persistent')
    await wrapper.get('#audit-category').setValue('control')
    await wrapper.get('#audit-result').setValue('succeeded')
    await wrapper.get('form.audit-filters').trigger('submit')
    await flushPromises()
    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/audit?after=0&limit=100&category=control&result=succeeded',
      expect.objectContaining({ headers: { Authorization: `Bearer ${token}` } }),
    )

    await wrapper.get('.history-actions button').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('Record 13')
  })

  it('shows a bounded empty degraded response without a ring fallback', async () => {
    const page = durableAuditPage()
    page.records = []
    page.cursor = 0
    page.oldestCursor = null
    const status = durableAuditStatus()
    status.audit.state = 'degraded'
    status.audit.degraded = true
    status.audit.lastError = 'audit_store_write_failed'
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/audit/status') return Promise.resolve(jsonResponse(status))
      if (url.startsWith('/api/v1/audit?')) return Promise.resolve(jsonResponse(page))
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(AuditWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.warning-notice').text()).toContain('Durable audit is degraded')
    expect(wrapper.get('.empty-list').text()).toContain('No durable audit records')
    expect(fetch.mock.calls.some(([input]) => String(input).includes('/api/v2/events'))).toBe(false)
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
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/audit/status') return Promise.resolve(jsonResponse(durableAuditStatus()))
      if (url.startsWith('/api/v1/audit?')) {
        return Promise.resolve(jsonResponse({ records: [{}] }))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    const malformed = mount(AuditWorkspace, { props: { token } })
    await flushPromises()
    expect(malformed.get('.error-notice').text()).toContain('invalid response payload')
    malformed.unmount()

    vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL) => {
      if (String(input).startsWith('/api/v1/audit/status')) return Promise.resolve(jsonResponse(durableAuditStatus()))
      return Promise.resolve(jsonResponse({ error: { code: 'route_not_found', message: 'route does not exist' } }, 404))
    }))
    const unavailable = mount(AuditWorkspace, { props: { token } })
    await flushPromises()
    expect(unavailable.get('.capability-panel').text()).toContain('Durable audit unavailable')
    expect(unavailable.get('.audit-actions button').attributes()).toHaveProperty('disabled')
  })
})
