import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import TopologyView from './TopologyView.vue'
import type { TopologySnapshot } from './api'

const topology: TopologySnapshot = {
  schemaVersion: 1,
  state: {
    config: 'active',
    runtime: 'active',
    sampledAtUnixMs: 1_750_000_000_000,
  },
  nodes: [
    node('listener:4:edge', 'listener', 'edge', '/listeners/0', {
      bind: { type: 'socket', address: '0.0.0.0:443' },
      protocol: 'http',
      service: 'web',
      tlsProfile: 'public',
      maxConnections: null,
    }),
    node('rtmp_listener:4:live', 'rtmp_listener', 'live', '/listeners/1', {
      bind: { type: 'unix', path: '/run/oxiroute/live.sock', mode: null },
      protocol: 'rtmp',
      maxConnections: 100,
      applications: [{
        name: 'live',
        live: true,
        idleStreams: true,
        recording: { supported: true, recorderCount: 1, manualRecorderCount: 1, continuousRecorderCount: 0 },
      }],
    }),
    node('tls_profile:6:public', 'tls_profile', 'public', '/tls_profiles/0', {
      minVersion: '1.2',
      certificates: ['public'],
    }),
    node('certificate:6:public', 'certificate', 'public', '/certificates/0', {
      dnsNames: ['example.test'],
      source: {
        type: 'files',
        certificateChainPath: '/etc/oxiroute/public.pem',
        privateKeyPath: '<redacted>',
      },
    }),
    node('http_service:3:web', 'http_service', 'web', '/http_services/0', {
      upstreamIoTimeoutMs: 30_000,
      maxRequestBodyBytes: null,
    }),
    node('http_route:3:web:0::1:/:0:', 'http_route', '* /', '/http_services/0/routes/0', {
      host: null,
      path: { kind: 'segment_prefix', value: '/' },
      methods: [],
      access: { type: 'bearer_token_file', headerName: 'authorization', realm: 'private' },
      action: {
        type: 'static_files',
        rootDirectory: '/srv/private-www',
        indexFiles: ['index.html'],
        spaFallback: true,
      },
      tokenFilePath: '/run/private/token',
    }),
    node('l4_service:8:database', 'l4_service', 'database', '/l4_services/0', {
      upstreamPool: 'database',
    }),
    node('upstream_pool:3:web', 'upstream_pool', 'web', '/upstream_pools/0', {
      algorithm: 'round_robin',
      caCertificatePath: '/etc/oxiroute/origin-ca.pem',
    }),
    node(
      'endpoint:3:web:14:127.0.0.1:3000',
      'endpoint',
      'backend.example.test:3000',
      '/upstream_pools/0/endpoints/0',
      { type: 'dns', host: 'backend.example.test', port: 3000 },
    ),
  ],
  edges: [
    edge('dispatch', 'dispatch_service', 'listener:4:edge', 'http_service:3:web'),
    edge('tls', 'listener_tls', 'listener:4:edge', 'tls_profile:6:public'),
    edge('certificate', 'tls_certificate', 'tls_profile:6:public', 'certificate:6:public'),
    edge('route', 'service_route', 'http_service:3:web', 'http_route:3:web:0::1:/:0:'),
    edge('route-pool', 'route_pool', 'http_route:3:web:0::1:/:0:', 'upstream_pool:3:web'),
    edge(
      'endpoint',
      'pool_endpoint',
      'upstream_pool:3:web',
      'endpoint:3:web:14:127.0.0.1:3000',
    ),
  ],
  overlays: [
    { nodeId: 'listener:4:edge', state: 'listening', metrics: { activeConnections: 7 } },
    {
      nodeId: 'rtmp_listener:4:live',
      state: 'configured',
      metrics: { activeConnections: 1, acceptedConnections: '1', bytesReceived: '1024', bytesSent: '0' },
    },
    {
      nodeId: 'upstream_pool:3:web',
      state: 'degraded',
      metrics: { availableEndpoints: 1, totalEndpoints: 2 },
    },
    {
      nodeId: 'endpoint:3:web:14:127.0.0.1:3000',
      state: 'healthy',
      metrics: { successfulChecks: '42', activeConnections: '3' },
    },
  ],
}

describe('TopologyView', () => {
  it.each([
    ['starting', 'Starting'],
    ['degraded', 'Degraded'],
  ] as const)('renders the %s runtime status and light truthfully', (runtime, label) => {
    const snapshot = structuredClone(topology)
    snapshot.state.runtime = runtime

    const wrapper = mount(TopologyView, { props: { topology: snapshot } })
    const status = wrapper.get('.topology-state')

    expect(status.classes()).toContain(`state-${runtime}`)
    expect(status.text()).toBe(`${label} / schema 1`)
    expect(status.attributes('aria-label')).toBe(`Runtime status: ${label}; topology schema 1`)
    expect(status.get('.state-light').classes()).toContain(`state-light-${runtime}`)
  })

  it('renders real nodes, typed connectors, distinct symbols, and runtime labels', () => {
    const wrapper = mount(TopologyView, { props: { topology } })

    expect(wrapper.findAll('.topology-node')).toHaveLength(topology.nodes.length)
    expect(wrapper.findAll('.connector')).toHaveLength(topology.edges.length)
    expect(wrapper.find('.node-listener path').exists()).toBe(true)
    expect(wrapper.find('.node-rtmp_listener circle').exists()).toBe(true)
    expect(wrapper.find('.node-tls_profile path').exists()).toBe(true)
    expect(wrapper.find('.node-certificate path').exists()).toBe(true)
    expect(wrapper.find('.node-http_service rect').exists()).toBe(true)
    expect(wrapper.find('.node-http_route path').exists()).toBe(true)
    expect(wrapper.find('.node-upstream_pool path').exists()).toBe(true)
    expect(wrapper.find('.node-endpoint circle').exists()).toBe(true)
    expect(wrapper.findAll('.topology-node').every((node) => Boolean(node.attributes('aria-label')))).toBe(true)
    expect(wrapper.findAll('.node-icon').every((icon) => icon.attributes('aria-hidden') === 'true')).toBe(true)
    expect(wrapper.findAll('.relation-link')).toHaveLength(topology.edges.length)
    expect(wrapper.get('.topology-relations').text()).toContain('edge')
    expect(wrapper.get('.topology-relations').text()).toContain('Dispatches to service')
    expect(wrapper.get('.state-degraded').text()).toContain('Degraded')
    expect(wrapper.get('.state-healthy').text()).toContain('Healthy')
    expect(wrapper.text()).toContain('Socket / 0.0.0.0:443')
    expect(wrapper.text()).toContain('DNS / backend.example.test:3000')
    expect(wrapper.text()).toContain('7 active connections')
    expect(wrapper.text()).toContain('3 active connections')
    expect(wrapper.find('.inspector').exists()).toBe(false)
  })

  it('opens a redacted inspector and supports arrow-key focus navigation', async () => {
    const wrapper = mount(TopologyView, { props: { topology }, attachTo: document.body })
    const certificate = wrapper.get('[data-node-id="certificate:6:public"]')

    await certificate.trigger('click')

    const inspector = wrapper.get('.inspector')
    expect(inspector.text()).toContain('/certificates/0')
    expect(inspector.text()).toContain('certificate:6:public')
    expect(inspector.text()).toContain('privateKeyPath')
    expect(inspector.text()).toContain('<redacted>')
    expect(inspector.text()).toContain('/etc/oxiroute/public.pem')
    expect(inspector.text()).not.toContain('private-key.pem')

    const rtmpListener = wrapper.get('[data-node-id="rtmp_listener:4:live"]')
    await rtmpListener.trigger('click')
    expect(wrapper.get('.inspector').text()).toContain('manualRecorderCount')

    const first = wrapper.get('.topology-node')
    ;(first.element as HTMLButtonElement).focus()
    await first.trigger('keydown', { key: 'ArrowRight' })
    expect(document.activeElement).toBe(wrapper.findAll('.topology-node')[1]!.element)

    await wrapper.get('.inspector-close').trigger('click')
    expect(wrapper.find('.inspector').exists()).toBe(false)
    expect(document.activeElement).toBe(rtmpListener.element)

    await wrapper.findAll('.relation-link')[0]!.trigger('click')
    expect(wrapper.get('.inspector').text()).toContain('web')

    await wrapper.get('[data-node-id="upstream_pool:3:web"]').trigger('click')
    expect(wrapper.get('.inspector').text()).toContain('/etc/oxiroute/origin-ca.pem')

    await wrapper.get('[data-node-id="listener:4:edge"]').trigger('click')
    expect(wrapper.get('.inspector-identity').text()).toContain('Connection limit')
    expect(wrapper.get('.inspector-identity').text()).toContain('Unbounded')
    expect(wrapper.get('.inspector-identity').text()).toContain('Active connections')

    await wrapper.get('[data-node-id="endpoint:3:web:14:127.0.0.1:3000"]').trigger('click')
    expect(wrapper.get('.inspector-identity').text()).toContain('DNS / backend.example.test:3000')
    expect(wrapper.get('.inspector-identity').text()).toContain('Active connections')

    await wrapper.get('[data-node-id="http_service:3:web"]').trigger('click')
    expect(wrapper.get('.inspector-identity').text()).toContain('Request body limit')
    expect(wrapper.get('.inspector-identity').text()).toContain('Unbounded')

    await wrapper.get('[data-node-id="http_route:3:web:0::1:/:0:"]').trigger('click')
    expect(wrapper.get('.inspector').text()).not.toContain('tokenFilePath')
    expect(wrapper.get('.inspector').text()).not.toContain('/run/private/token')
    expect(wrapper.get('.inspector').text()).not.toContain('rootDirectory')
    expect(wrapper.get('.inspector').text()).not.toContain('/srv/private-www')
    wrapper.unmount()
  })
})

function node(
  id: string,
  kind: TopologySnapshot['nodes'][number]['kind'],
  name: string,
  configPath: string,
  attributes: TopologySnapshot['nodes'][number]['attributes'],
): TopologySnapshot['nodes'][number] {
  return { id, kind, name, configPath, attributes }
}

function edge(
  id: string,
  kind: TopologySnapshot['edges'][number]['kind'],
  source: string,
  target: string,
): TopologySnapshot['edges'][number] {
  return { id, kind, source, target, configPath: `/references/${id}` }
}
