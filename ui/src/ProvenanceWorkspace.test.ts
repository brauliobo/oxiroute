import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import ProvenanceWorkspace from './ProvenanceWorkspace.vue'
import { fetchImportReports } from './api'
import { jsonResponse } from './test/contractFixtures'
import { importReportResponse } from './test/importFixtures'

const token = 'native-report-test-token'

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})

describe('native import report workspace', () => {
  it('renders the product contract, redacted source graph, provenance, and preview', async () => {
    const report = importReportResponse()
    const fetch = reportFetch(report)
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.report-identity').text()).toContain('apache import')
    expect(wrapper.get('.report-identity').text()).toContain('apache-static-reverse-proxy')
    expect(wrapper.get('.evidence-panel').text()).toContain('Source 1')
    expect(wrapper.get('.evidence-panel').text()).not.toContain('/etc/ssl')
    expect(wrapper.get('.provenance-panel').text()).toContain('/http_services/0/routes/0/action')
    expect(wrapper.get('.preview-panel pre').text()).toContain('version 1')
    expect(wrapper.text()).not.toContain('private-key-secret')
    expect(fetch.mock.calls.every(([, init]) => init?.headers === undefined ||
      (init.headers as Record<string, string>).Authorization === `Bearer ${token}`)).toBe(true)
    wrapper.unmount()
  })

  it('keeps blocked candidates fail-closed without rendering a preview', async () => {
    const report = importReportResponse(true)
    vi.stubGlobal('fetch', reportFetch(report))

    const wrapper = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('.status-chip').text()).toBe('blocked')
    expect(wrapper.get('.blockers-panel').text()).toContain('E_REWRITE_UNSUPPORTED')
    expect(wrapper.findAll('.preview-panel pre')).toHaveLength(0)
    expect(wrapper.get('.blocked-preview').text()).toContain('No preview was produced')
    wrapper.unmount()
  })

  it('retains the visible report when a newly selected result has a newer disk revision', async () => {
    const oldReport = importReportResponse()
    oldReport.reports.push({
      ...oldReport.reports[0]!,
      index: 1,
      product: 'nginx',
      capabilityProfile: { id: 'nginx-root', version: 1 },
    })
    const newReport = importReportResponse()
    newReport.diskRevision = 'new-disk-revision'
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/import-reports') return Promise.resolve(jsonResponse(oldReport))
      if (url === '/api/v1/import-reports/0') return Promise.resolve(jsonResponse(oldReport))
      if (url === '/api/v1/import-reports/1') return Promise.resolve(jsonResponse(newReport))
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()
    await wrapper.get('#import-report-selection').setValue('1')
    await flushPromises()

    expect(wrapper.get('.stale-banner').text()).toContain('disk revision changed')
    expect(wrapper.get('.report-identity').text()).toContain('apache import')
    expect(wrapper.findAll('.preview-panel pre')).toHaveLength(1)
    wrapper.unmount()
  })

  it('uses bearer auth and fails closed on unauthorized or malformed API responses', async () => {
    const unauthorizedFetch = vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'unauthorized', message: 'invalid bearer token' },
    }, 401)))
    vi.stubGlobal('fetch', unauthorizedFetch)

    await expect(fetchImportReports(token)).rejects.toMatchObject({ status: 401, code: 'unauthorized' })
    expect(unauthorizedFetch).toHaveBeenCalledWith('/api/v1/import-reports', expect.objectContaining({
      cache: 'no-store',
      headers: { Authorization: `Bearer ${token}` },
    }))

    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({ reports: [] }))))
    await expect(fetchImportReports(token)).rejects.toThrow('native import reports API returned an invalid response payload')
  })
})

function reportFetch(report: ReturnType<typeof importReportResponse>) {
  return vi.fn((input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/import-reports' || url === '/api/v1/import-reports/0') {
      return Promise.resolve(jsonResponse(report))
    }
    throw new Error(`Unexpected request: ${url}`)
  })
}
