import { describe, expect, it } from 'vitest'
import { importReportResponse } from '../test/importFixtures'
import { mount } from '../test/mount'
import EvidencePanel from './EvidencePanel.vue'
import {
  blockerRows,
  dependencyRows,
  diagnosticRows,
  originRows,
  overlayRows,
  reportMetrics,
  requirementRows,
  selectionOptions,
  sourceRows,
  statusBadge,
  versionText,
} from './model'

const blocked = importReportResponse(true).report!
const finalized = importReportResponse().report!

describe('provenance model', () => {
  it('turns each evidence list into rows of words with IEC sizes', () => {
    expect(sourceRows(finalized)).toEqual([['Source 1', 'source-1', '128 B'], ['Source 2', 'source-2', '256 B']])
    expect(dependencyRows(finalized)).toEqual([['include', 'Source 1', 'Source 2', 'Expanded']])
    expect(originRows(finalized)).toEqual([['/http_services/0/routes/0/action', '1 origin', 'proxy_pass / source 2']])
    expect(requirementRows(finalized)).toEqual([
      ['SSLCertificateFile', 'Certificate material', '1 value redacted'],
      ['Listen', 'Listener mode', 'No values supplied'],
    ])
    expect(overlayRows(finalized)).toEqual([['certificate-material', 'Certificate material', 'Satisfied']])
    expect(blockerRows(blocked)[0]).toEqual([
      'E_REWRITE_UNSUPPORTED', 'Rewrite behavior is outside the exact canonical subset.', 'blocked.example.test',
    ])
    expect(diagnosticRows(blocked)[0]).toEqual([
      'E_REWRITE_UNSUPPORTED', 'Rewrite behavior is outside the exact canonical subset.', 'Lowering', 'Error',
    ])
  })

  it('summarizes counts, version and status', () => {
    expect(reportMetrics(finalized).map(({ label, value }) => `${label}: ${value}`)).toEqual([
      'Sources: 2', 'Dependencies: 1', 'Provenance paths: 1', 'Requirements: 2', 'Diagnostics: 0',
    ])
    expect(versionText(finalized)).toBe('Source version not supplied')
    expect(statusBadge('blocked')).toEqual({ tone: 'bad', label: 'Blocked' })
    expect(statusBadge('finalized').tone).toBe('ok')
    expect(selectionOptions(importReportResponse().reports)).toEqual([
      { value: '0', label: 'apache / apache-static-reverse-proxy / finalized' },
    ])
  })
})

describe('EvidencePanel', () => {
  const props = { title: 'Sources', note: 'Redacted.', caption: 'Source files', columns: ['Source', 'Name'], empty: 'Nothing.' }

  it('renders rows as a labelled table with the first cell as the row header', () => {
    const wrapper = mount(EvidencePanel, { props: { ...props, rows: [['Source 1', 'a'], ['Source 2', 'b']] } })
    expect(wrapper.get('h2').text()).toBe('Sources')
    expect(wrapper.get('[role="region"]').attributes('aria-label')).toBe('Source files table')
    expect(wrapper.findAll('tbody th').map((cell) => cell.text())).toEqual(['Source 1', 'Source 2'])
    expect(wrapper.get('tbody td').attributes('data-label')).toBe('Name')
  })

  it('shows the empty message instead of a table', () => {
    const wrapper = mount(EvidencePanel, { props: { ...props, rows: [] } })
    expect(wrapper.find('table').exists()).toBe(false)
    expect(wrapper.text()).toContain('Nothing.')
  })
})
