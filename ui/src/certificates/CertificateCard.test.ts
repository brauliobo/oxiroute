import { describe, expect, it } from 'vitest'
import type { TlsCertificateInventory } from '../api'
import { mount } from '../test/mount'
import { managementTlsInventory } from '../test/managementFixtures'
import CertificateCard from './CertificateCard.vue'

const NOW = Date.parse('2026-07-12T00:00:00Z')

function certificate(name: string): TlsCertificateInventory {
  return managementTlsInventory().certificates.find((entry) => entry.name === name)!
}

describe('CertificateCard', () => {
  it('shows UTC times, the relative expiry and a warning badge with word and shape', () => {
    const wrapper = mount(CertificateCard, { props: { certificate: certificate('managed-edge'), nowMs: NOW, disabled: false } })

    expect(wrapper.get('h3').text()).toBe('managed-edge')
    expect(wrapper.text()).toContain('ACME managed')
    expect(wrapper.text()).toContain('2026-08-01 00:00:00 UTC, expires in 20 days')
    expect(wrapper.text()).toContain('Next action2025-07-08 18:40:00 UTC')
    const badges = wrapper.findAll('.ox-badge')
    expect(badges.map((badge) => badge.text())).toEqual(['Loaded', 'Expires soon'])
    expect(badges[1]!.classes()).toContain('ox-tone-warn')
    expect(badges[1]!.find('svg path').exists()).toBe(true)
  })

  it('offers the managed actions in a stable order and emits the chosen one', async () => {
    const wrapper = mount(CertificateCard, { props: { certificate: certificate('managed-edge'), nowMs: NOW, disabled: false } })

    expect(wrapper.findAll('.ox-row-actions button').map((button) => button.text())).toEqual([
      'Reconcile status', 'Renew now', 'Rollover account key', 'Revoke certificate', 'Delete stored state', 'Pause renewal',
    ])
    await wrapper.findAll('.ox-row-actions button')[1]!.trigger('click')
    expect(wrapper.emitted('act')).toEqual([['renew']])
  })

  it('offers resume and cancel for a paused certificate with an active job', () => {
    const paused = certificate('managed-edge')
    Object.assign(paused.status!, { paused: true, jobId: 'job-1' })
    const wrapper = mount(CertificateCard, { props: { certificate: paused, nowMs: NOW, disabled: false } })

    const labels = wrapper.findAll('.ox-row-actions button').map((button) => button.text())
    expect(labels).toContain('Resume renewal')
    expect(labels).toContain('Cancel active job')
    expect(wrapper.text()).toContain('Renewal paused')
    expect(wrapper.text()).toContain('job-1')
  })

  it('has no actions for a development identity and disables them while busy', () => {
    const development = mount(CertificateCard, { props: { certificate: certificate('development-edge'), nowMs: NOW, disabled: false } })
    expect(development.find('.ox-row-actions').exists()).toBe(false)
    expect(development.text()).toContain('Development only')

    const busy = mount(CertificateCard, { props: { certificate: certificate('managed-edge'), nowMs: NOW, disabled: true } })
    expect(busy.findAll('.ox-row-actions button').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
  })

  it('reports an expired certificate and a missing material', () => {
    const expired = certificate('development-edge')
    expired.status = { ...expired.status!, expiresAt: '2026-07-09T00:00:00Z' } as typeof expired.status
    const wrapper = mount(CertificateCard, { props: { certificate: expired, nowMs: NOW, disabled: false } })
    expect(wrapper.findAll('.ox-badge').map((badge) => badge.text())).toContain('Expired')
    expect(wrapper.text()).toContain('expired 3 days ago')

    const missing = { ...certificate('development-edge'), status: null }
    const empty = mount(CertificateCard, { props: { certificate: missing, nowMs: NOW, disabled: false } })
    expect(empty.get('.ox-badge').text()).toBe('No active material')
    expect(empty.get('.ox-badge').classes()).toContain('ox-tone-bad')
  })
})
