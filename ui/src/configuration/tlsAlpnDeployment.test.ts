import { describe, expect, it } from 'vitest'

import { prepareTlsAlpnDeployment } from './tlsAlpnDeployment'
import { emptyConfigSnapshot } from '../test/contractFixtures'

describe('TLS-ALPN deployment assistant', () => {
  it('adds a reviewable HTTP listener, TLS profile, and isolated 404 service', () => {
    const config = emptyConfigSnapshot().config
    config.certificates.push({
      name: 'managed-edge',
      dns_names: ['edge.example.test'],
      source: {
        type: 'acme_managed',
        directory_url: 'https://acme.example.test/directory',
        state_root: '/var/lib/oxiroute/acme',
        contacts: [],
        terms_agreed: true,
        challenge: 'tls_alpn01',
        key_type: 'ecdsa_p256',
        allowed_dns_suffixes: ['example.test'],
        retained_revisions: 3,
        retention_days: 30,
        dns01: null,
      },
    })

    const result = prepareTlsAlpnDeployment(config, 'managed-edge')

    expect(result).toMatchObject({
      outcome: 'prepared',
      listenerName: 'managed-edge-tls-alpn-listener',
      profileName: 'managed-edge-tls-alpn-profile',
      serviceName: 'managed-edge-tls-alpn-service',
      bindAddress: '0.0.0.0:443',
    })
    expect(config.listeners).toHaveLength(1)
    expect(config.listeners[0]).toMatchObject({
      protocol: 'http',
      service: 'managed-edge-tls-alpn-service',
      tls_profile: 'managed-edge-tls-alpn-profile',
      bind: { type: 'socket', address: '0.0.0.0:443' },
    })
    expect(config.tls_profiles[0]).toMatchObject({
      certificates: ['managed-edge'],
      default_certificate: 'managed-edge',
      alpn: ['h2', 'http/1.1'],
    })
    expect(config.http_services[0]?.routes[0]?.action).toEqual({
      type: 'fixed_response',
      status: 404,
      body: '',
      headers: [],
    })
  })

  it('reuses a compatible existing TLS listener without adding objects', () => {
    const config = emptyConfigSnapshot().config
    config.certificates.push(managedCertificate())
    config.tls_profiles.push({
      name: 'public',
      certificates: ['managed-edge'],
      default_certificate: 'managed-edge',
      min_version: '1.2',
      alpn: ['http/1.1'],
      policy: tlsPolicy(),
    })
    config.listeners.push({
      name: 'public-https',
      bind: { type: 'socket', address: '[::]:443' },
      protocol: 'http',
      service: 'web',
      tls_profile: 'public',
      max_connections: 10_000,
      downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
    })

    const result = prepareTlsAlpnDeployment(config, 'managed-edge')

    expect(result).toEqual({
      outcome: 'ready',
      listenerName: 'public-https',
      profileName: 'public',
      bindAddress: '[::]:443',
    })
    expect(config.listeners).toHaveLength(1)
    expect(config.tls_profiles).toHaveLength(1)
  })

  it('refuses to claim a plaintext port already owned by the draft', () => {
    const config = emptyConfigSnapshot().config
    config.certificates.push(managedCertificate())
    config.listeners.push({
      name: 'plain-https',
      bind: { type: 'socket', address: '127.0.0.1:443' },
      protocol: 'http',
      service: 'web',
      tls_profile: null,
      max_connections: 10_000,
      downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
    })

    const result = prepareTlsAlpnDeployment(config, 'managed-edge')

    expect(result).toMatchObject({ outcome: 'blocked' })
    expect(result.outcome === 'blocked' && result.message).toContain('already owns')
    expect(config.tls_profiles).toHaveLength(0)
    expect(config.listeners).toHaveLength(1)
  })
})

function managedCertificate() {
  return {
    name: 'managed-edge',
    dns_names: ['edge.example.test'],
    source: {
      type: 'acme_managed' as const,
      directory_url: 'https://acme.example.test/directory',
      state_root: '/var/lib/oxiroute/acme',
      contacts: [],
      terms_agreed: true,
      challenge: 'tls_alpn01' as const,
      key_type: 'ecdsa_p256' as const,
      allowed_dns_suffixes: ['example.test'],
      retained_revisions: 3,
      retention_days: 30,
      dns01: null,
    },
  }
}

function tlsPolicy() {
  return {
    cipher_list: null,
    dh_parameters_path: null,
    client_auth: { mode: 'disabled' as const, ca_certificate_path: null, allowed_dns_names: [] },
    session_cache: null,
    session_timeout_seconds: null,
    session_tickets: false,
    prefer_server_ciphers: true,
  }
}
