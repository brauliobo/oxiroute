import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { ListenerConfig } from '../config'
import { contractConfigSnapshot } from '../test/contractFixtures'
import CacheStoreEditor from './CacheStoreEditor.vue'
import { defaultForwardProxyService, defaultHttpCachePolicy } from './canonicalDefaults'
import ForwardProxyServiceEditor from './ForwardProxyServiceEditor.vue'
import HttpCachePolicyEditor from './HttpCachePolicyEditor.vue'
import ListenerEditor from './ListenerEditor.vue'
import TlsProfileEditor from './TlsProfileEditor.vue'

describe('cache and forward proxy editors', () => {
  it('replaces cache store variants without retaining variant-only fields', async () => {
    const store = contractConfigSnapshot().config.cache_stores[0]!
    const wrapper = mount(CacheStoreEditor, { props: { store } })

    await wrapper.get('[data-field="cache_stores[].type"] select').setValue('disk')

    expect(wrapper.emitted('replace')).toEqual([[expect.objectContaining({
      type: 'disk',
      name: 'memory-responses',
      root_directory: '',
      max_files: 1_000_000,
    })]])
    expect(wrapper.emitted('replace')?.[0]?.[0]).not.toHaveProperty('max_entries')

    const disk = contractConfigSnapshot().config.cache_stores[1]!
    const diskWrapper = mount(CacheStoreEditor, { props: { store: disk } })
    expect((diskWrapper.get('[data-field="cache_stores[].root_directory"] input').element as HTMLInputElement).value)
      .toBe('/var/cache/oxiroute')
  })

  it('uses finite canonical cache defaults and preserves file-backed purge authorization', async () => {
    const policy = contractConfigSnapshot().config.http_services[0]!.routes[0]!.action
    if (policy.type !== 'proxy') throw new Error('fixture route must be a proxy')
    const wrapper = mount(HttpCachePolicyEditor, {
      props: { policy: policy.policy, storeNames: ['memory-responses', 'responses'] },
    })

    expect(policy.policy.cache).toEqual(expect.objectContaining({
      store: 'responses',
      default_ttl_ms: 60_000,
      purge_authorization: {
        type: 'bearer_token_file',
        token_file_path: '/run/oxiroute/cache-purge.token',
      },
    }))
    expect((wrapper.get('[data-field="http_services[].routes[].action.policy.cache.purge_authorization.token_file_path"] input').element as HTMLInputElement).value)
      .toBe('/run/oxiroute/cache-purge.token')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.cache"] input').setValue(false)
    await wrapper.get('[data-field="http_services[].routes[].action.policy.cache"] input').setValue(true)
    expect(policy.policy.cache).toEqual(defaultHttpCachePolicy('memory-responses'))
  })

  it('edits every forward proxy group while retaining finite defaults', async () => {
    const service = defaultForwardProxyService()
    const wrapper = mount(ForwardProxyServiceEditor, { props: { service } })

    await wrapper.get('[data-field="forward_proxy_services[].name"] input').setValue('egress')
    await wrapper.get('[data-field="forward_proxy_services[].connect.enabled"] input').setValue(true)
    await wrapper.get('[data-field="forward_proxy_services[].auth"] input').setValue(true)
    await wrapper.get('[data-field="forward_proxy_services[].auth.token_file_path"] input').setValue('/run/forward.token')

    expect(service).toEqual(expect.objectContaining({
      name: 'egress',
      connect: { enabled: true, allowed_ports: [443] },
      auth: { type: 'bearer_token_file', token_file_path: '/run/forward.token' },
      max_request_body_bytes: 10_485_760,
      audit_mode: 'metadata',
    }))

    await wrapper.get('[data-field="forward_proxy_services[].auth.type"] select').setValue('basic_htpasswd_file')
    const ttl = wrapper.get('[data-field="forward_proxy_services[].auth.credential_ttl_ms"] input')
    await ttl.setValue('250')
    expect(service.auth).toEqual(expect.objectContaining({ credential_ttl_ms: 250 }))
    await ttl.setValue('')
    expect(service.auth).toEqual(expect.objectContaining({ credential_ttl_ms: null }))
  })

  it('normalizes an HTTP/3 forward listener to UDP and an exact H3 TLS profile', async () => {
    const forward = defaultForwardProxyService()
    forward.name = 'egress'
    forward.enabled_versions = ['h1', 'h2', 'h3']
    const listener: ListenerConfig = {
      name: 'forward',
      bind: { type: 'socket', address: '0.0.0.0:8080' },
      protocol: 'forward_http1',
      service: 'egress',
      tls_profile: null,
      max_connections: 1_000,
      downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
    }
    const wrapper = mount(ListenerEditor, {
      props: {
        listener,
        httpServiceNames: [],
        rtmpServiceNames: [],
        l4ServiceNames: [],
        forwardProxyServices: [forward],
        tlsProfiles: [{
          name: 'forward-h3',
          certificates: ['forward'],
          default_certificate: 'forward',
          min_version: '1.3',
          alpn: ['h3'],
        }],
      },
    })

    await wrapper.get('[data-field="listeners[].protocol"] select').setValue('forward_http3')

    expect(listener.bind).toEqual({ type: 'udp', address: '0.0.0.0:443' })
    expect(listener.tls_profile).toBe('forward-h3')
    expect(wrapper.get('[data-field="listeners[].bind.type"] option[value="socket"]').attributes())
      .toHaveProperty('disabled')
    expect(wrapper.get('[data-field="listeners[].tls_profile"] select').findAll('option')[0]!.attributes())
      .toHaveProperty('disabled')
  })

  it('forces TLS 1.3 when selecting H3 ALPN', async () => {
    const profile = {
      name: 'forward',
      certificates: ['forward'],
      default_certificate: 'forward',
      min_version: '1.2' as const,
      alpn: ['http/1.1' as const],
    }
    const wrapper = mount(TlsProfileEditor, {
      props: { profile, certificateNames: ['forward'] },
    })

    await wrapper.get('[data-field="tls_profiles[].alpn"] select').setValue('h3')

    expect(profile.alpn).toEqual(['h3'])
    expect(profile.min_version).toBe('1.3')
    expect(wrapper.get('[data-field="tls_profiles[].min_version"] option[value="1.2"]').attributes())
      .toHaveProperty('disabled')
  })
})
