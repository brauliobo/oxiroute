import { flushPromises, mount } from './test/mount'
import { afterEach, describe, expect, it, vi } from 'vitest'

import ProvenanceWorkspace from './ProvenanceWorkspace.vue'
import { fetchImportReports } from './api'
import { contractConfigSnapshot, jsonResponse } from './test/contractFixtures'
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

    expect(panel(wrapper, 'apache import: apache-static-reverse-proxy').text()).toContain('Finalized')
    expect(panel(wrapper, 'Resolved inputs').text()).toContain('Source 1')
    expect(panel(wrapper, 'Resolved inputs').text()).toContain('128 B')
    expect(panel(wrapper, 'Resolved inputs').text()).not.toContain('/etc/ssl')
    expect(panel(wrapper, 'Source dependencies').text()).toContain('Source 2')
    expect(panel(wrapper, 'Canonical field origins').text()).toContain('/http_services/0/routes/0/action')
    expect(panel(wrapper, 'Canonical field origins').text()).toContain('1 origin')
    expect(panel(wrapper, 'Requirements').text()).toContain('1 value redacted')
    expect(wrapper.get('pre').text()).toContain('version 1')
    expect(wrapper.text()).not.toContain('Management token required')
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

    expect(panel(wrapper, 'apache import: apache-static-reverse-proxy').text()).toContain('Blocked')
    expect(panel(wrapper, 'Blockers').text()).toContain('E_REWRITE_UNSUPPORTED')
    expect(wrapper.findAll('pre')).toHaveLength(0)
    expect(wrapper.text()).toContain('No preview was produced.')
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

    expect(wrapper.text()).toContain('Disk revision changed.')
    expect(panel(wrapper, 'apache import: apache-static-reverse-proxy').exists()).toBe(true)
    expect(wrapper.findAll('pre')).toHaveLength(1)
    wrapper.unmount()
  })

  it('shows an honest empty state and says one report in the singular', async () => {
    const empty = importReportResponse()
    empty.reports = []
    empty.selection = null
    empty.report = null
    empty.preview = null
    vi.stubGlobal('fetch', reportFetch(empty))
    const none = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()
    expect(none.text()).toContain('No import reports are available')
    expect(none.text()).toContain('Native sources remain read-only.')
    none.unmount()

    vi.stubGlobal('fetch', reportFetch(importReportResponse()))
    const one = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()
    expect(one.text()).toContain('1 report retained')
    expect(one.text()).not.toContain('1 reports')
    expect(one.findAll('#import-report-selection option').map((option) => option.text())).toEqual([
      'apache / apache-static-reverse-proxy / finalized',
    ])
  })

  it('gives every evidence table a labelled scroll region that the keyboard reaches', async () => {
    vi.stubGlobal('fetch', reportFetch(importReportResponse(true)))
    const wrapper = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()

    const regions = wrapper.findAll('[role="region"]')
    expect(regions.length).toBeGreaterThan(4)
    expect(regions.every((region) => region.attributes('tabindex') === '0' && region.attributes('aria-label'))).toBe(true)
    expect(new Set(regions.map((region) => region.attributes('aria-label'))).size).toBe(regions.length)
  })

  it('relocks on an unauthorized response', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({ error: { code: 'unauthorized', message: 'no' } }, 401))))
    const wrapper = mount(ProvenanceWorkspace, { props: { token } })
    await flushPromises()
    expect(wrapper.emitted('unauthorized')).toBeTruthy()
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

  it.each([
    { finalized: true, config: null },
    { finalized: false, config: contractConfigSnapshot().config },
  ])('rejects inconsistent candidate finalization evidence: %o', async (candidate) => {
    const response = importReportResponse()
    Object.assign(response.report!.candidate, candidate)
    vi.stubGlobal('fetch', reportFetch(response))

    await expect(fetchImportReports(token)).rejects.toThrow(
      'native import reports API returned an invalid response payload',
    )
  })
})

function panel(wrapper: ReturnType<typeof mount>, title: string) {
  return wrapper.findAll('section').find((section) => section.find('h2').text() === title)!
}

function reportFetch(report: ReturnType<typeof importReportResponse>) {
  return vi.fn((input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/import-reports' || url === '/api/v1/import-reports/0') {
      return Promise.resolve(jsonResponse(report))
    }
    throw new Error(`Unexpected request: ${url}`)
  })
}
