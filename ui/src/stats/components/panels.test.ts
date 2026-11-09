import { describe, expect, it } from 'vitest'
import { counter } from '../fixtures'
import CertificatesPanel from './CertificatesPanel.vue'
import GenerationPanel from './GenerationPanel.vue'
import ProcessPanel from './ProcessPanel.vue'
import { documentAt, loadedStore, mountView } from './testing'
import WarningsPanel from './WarningsPanel.vue'

describe('CertificatesPanel', () => {
  it('lists certificates soonest first with a word for their state, and the renewal watchers', async () => {
    const wrapper = mountView(CertificatesPanel, await loadedStore([documentAt(0, (wire) => {
      const base = wire.sampledAtUnixMs / 1000
      wire.certificates.push(
        { name: 'soon', source: 'certbot', expiresAt: 'Jan 1', notAfterUnixSeconds: base + 5 * 86_400, lastOutcome: 'renewed', lastErrorCode: 'E_DNS' },
        { name: 'unreadable', source: 'acme_managed', expiresAt: 'unknown' },
      )
      wire.certificateWatchers = [{ source: 'certbot', health: 'degraded', backendErrors: counter(3), reconciliationFailures: counter(0) }]
    })]))
    expect(wrapper.findAll('table')[0]!.findAll('tbody th').map((item) => item.text())).toEqual(['soon', 'edge', 'unreadable'])
    expect(wrapper.findAll('tbody .ox-badge').map((item) => item.text())).toEqual(['EXPIRES SOON', 'VALID', 'UNKNOWN', 'DEGRADED'])
    expect(wrapper.text()).toContain('in 5d 0h')
    expect(wrapper.text()).toContain('E_DNS')
    expect(wrapper.get('h3').text()).toBe('Renewal watchers')
  })

  it('says so when no certificate is loaded', async () => {
    const wrapper = mountView(CertificatesPanel, await loadedStore([documentAt(0, (wire) => { wire.certificates = [] })]))
    expect(wrapper.text()).toContain('No certificates are loaded.')
  })
})

describe('WarningsPanel', () => {
  it('lists warnings as text with their code', async () => {
    const wrapper = mountView(WarningsPanel, await loadedStore([documentAt(0, (wire) => {
      wire.warnings.push({ code: 'W_X', message: '<img src=x onerror=1>' })
    })]))
    expect(wrapper.findAll('li')).toHaveLength(2)
    expect(wrapper.text()).toContain('W_PASSIVE_HEALTH_MISSING')
    expect(wrapper.text()).toContain('<img src=x onerror=1>')
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.get('li .ox-badge').text()).toBe('WARNING')
  })

  it('says so when there are none', async () => {
    const wrapper = mountView(WarningsPanel, await loadedStore([documentAt(0, (wire) => { wire.warnings = [] })]))
    expect(wrapper.text()).toContain('No configuration warnings.')
  })
})

describe('GenerationPanel', () => {
  it('shows revisions with an em dash for the ones not sent, counters, rejection and draining generations', async () => {
    const wrapper = mountView(GenerationPanel, await loadedStore([documentAt(0, (wire) => {
      wire.generation.lastRejection = { atUnixMs: wire.sampledAtUnixMs - 1000, revision: 'rev-5', diagnostics: [{ code: 'E_BAD', stage: 'validate', message: 'bad pool' }] }
      wire.generation.retired = [{ revision: 'rev-3', state: 'draining', ageSeconds: 90, deadlineRemainingSeconds: 30, inFlight: 5, inFlightByKind: { http: 4, relay: 1 } }]
    })]))
    expect(wrapper.get('h2').text()).toBe('Generation 4')
    expect(wrapper.text()).toContain('Activerev-4')
    expect(wrapper.text()).toContain('Candidate—')
    expect(wrapper.text()).toContain('Prepares5')
    expect(wrapper.text()).toContain('E_BAD')
    expect(wrapper.text()).toContain('bad pool')
    expect(wrapper.get('tbody').text()).toContain('rev-3')
    expect(wrapper.get('tbody').text()).toContain('http 4, relay 1')
    expect(wrapper.findAll('.ox-badge').map((item) => item.text())).toEqual(['ACCEPTING', 'HEALTHY'])
  })

  it('says that a generation is not accepting or degraded in words', async () => {
    const wrapper = mountView(GenerationPanel, await loadedStore([documentAt(0, (wire) => {
      wire.generation.accepting = false
      wire.generation.degraded = true
    })]))
    expect(wrapper.findAll('.ox-badge').map((item) => item.text())).toEqual(['NOT ACCEPTING', 'DEGRADED'])
  })
})

describe('ProcessPanel', () => {
  it('shows the process, its connections, the work in flight and the host', async () => {
    const wrapper = mountView(ProcessPanel, await loadedStore([documentAt(0, (wire) => { delete wire.process.cpuPercent })]))
    const text = wrapper.text()
    for (const part of ['Version6.3.0', 'CPU—', 'Resident memory143 MiB', 'Threads8', 'Open connections7', 'Retry attempts10',
      'Overdue0', 'Load 1m0.50', 'Memory total7.5 GiB']) expect(text).toContain(part)
    expect(wrapper.findAll('.ox-badge').map((item) => item.text())).toEqual(['HEALTHY', 'HEALTHY'])
  })

  it('names a degraded process and its reason', async () => {
    const wrapper = mountView(ProcessPanel, await loadedStore([documentAt(0, (wire) => {
      wire.process.state = 'degraded'
      wire.process.stateReason = 'memory pressure'
    })]))
    expect(wrapper.findAll('.ox-badge')[0]!.text()).toBe('DEGRADED')
    expect(wrapper.text()).toContain('memory pressure')
  })
})

