import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type {
  AcmeManagedCertificateSource,
  CertificateConfig,
} from '../config'
import CertificateEditor from './CertificateEditor.vue'

function filesCertificate(): CertificateConfig {
  return {
    name: 'edge-certificate',
    dns_names: ['edge.example.test'],
    source: {
      type: 'files',
      certificate_chain_path: '',
      private_key_path: '',
    },
  }
}

function managedCertificate(): CertificateConfig {
  const source: AcmeManagedCertificateSource = {
    type: 'acme_managed',
    directory_url: 'https://acme.example.test/directory',
    state_root: '/var/lib/oxiroute/acme',
    contacts: ['mailto:ops@example.test'],
    terms_agreed: true,
    challenge: 'http01',
    key_type: 'ecdsa_p256',
    allowed_dns_suffixes: ['example.test'],
    retained_revisions: 3,
    retention_days: 30,
    dns01: null,
  }
  return {
    name: 'edge-certificate',
    dns_names: ['edge.example.test'],
    source,
  }
}

describe('CertificateEditor ACME challenges', () => {
  it('offers TLS-ALPN-01 and defaults a new managed source to HTTP-01', async () => {
    const certificate = filesCertificate()
    const wrapper = mount(CertificateEditor, { props: { certificate } })

    await wrapper.get('[data-field="certificates[].source.type"] select').setValue('acme_managed')

    const challenge = wrapper.get('[data-field="certificates[].source.challenge"] select')
    expect((challenge.element as HTMLSelectElement).value).toBe('http01')
    expect(challenge.findAll('option').map((option) => option.attributes('value'))).toEqual([
      'http01',
      'dns01',
      'tls_alpn01',
    ])
    expect(certificate.source).toEqual(expect.objectContaining({ challenge: 'http01', dns01: null }))
    expect(wrapper.find('[data-field="certificates[].source.dns01.provider"]').exists()).toBe(false)
  })

  it('keeps DNS provider state only while DNS-01 is selected across every challenge transition', async () => {
    const certificate = managedCertificate()
    const wrapper = mount(CertificateEditor, { props: { certificate } })
    const challenge = wrapper.get('[data-field="certificates[].source.challenge"] select')
    const source = () => certificate.source as AcmeManagedCertificateSource
    const dnsProvider = '[data-field="certificates[].source.dns01.provider"]'

    await challenge.setValue('dns01')
    expect(source().dns01).toEqual({ provider: '', credential_file: '', timeout_seconds: 300 })
    await wrapper.get(`${dnsProvider} input`).setValue('route53')
    await wrapper.get('[data-field="certificates[].source.dns01.credential_file"] input').setValue('/run/dns-token')
    expect(source().dns01).toEqual({ provider: 'route53', credential_file: '/run/dns-token', timeout_seconds: 300 })

    await challenge.setValue('tls_alpn01')
    expect(source().challenge).toBe('tls_alpn01')
    expect(source().dns01).toBeNull()
    expect(wrapper.find(dnsProvider).exists()).toBe(false)
    expect(JSON.stringify(certificate.source)).not.toContain('route53')
    expect(wrapper.get('.challenge-note').text()).toContain('public TCP port 443')
    expect(wrapper.get('.challenge-note').text()).toContain('does not create or deploy')
    expect(challenge.attributes('aria-describedby')).toBe('tls-alpn-deployment-note')

    await challenge.setValue('http01')
    expect(source().challenge).toBe('http01')
    expect(source().dns01).toBeNull()

    await challenge.setValue('tls_alpn01')
    expect(source().challenge).toBe('tls_alpn01')
    expect(source().dns01).toBeNull()

    await challenge.setValue('dns01')
    expect(source().challenge).toBe('dns01')
    expect(source().dns01).toEqual({ provider: '', credential_file: '', timeout_seconds: 300 })
    expect(wrapper.find(dnsProvider).exists()).toBe(true)

    await challenge.setValue('http01')
    expect(source().challenge).toBe('http01')
    expect(source().dns01).toBeNull()
    expect(wrapper.find(dnsProvider).exists()).toBe(false)
  })
})
