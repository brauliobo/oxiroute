import { mount } from './test/mount'
import { beforeEach, describe, expect, it, vi } from 'vitest'

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
  beforeEach(() => {
    Element.prototype.scrollIntoView = vi.fn()
  })

  it('positions the stage and nodes with custom properties instead of inline style bindings', () => {
    const wrapper = mount(TopologyView, { props: { topology } })
    const property = (element: Element, name: string) => (element as HTMLElement).style.getPropertyValue(name)

    for (const name of ['--stage-width', '--stage-height', '--node-w', '--node-h']) {
      expect(property(wrapper.get('.ox-topology-stage').element, name)).toMatch(/^\d+px$/)
    }
    const slots = wrapper.findAll('.ox-node-slot')
    expect(slots).toHaveLength(topology.nodes.length)
    for (const slot of slots) {
      expect(property(slot.element, '--node-x')).toMatch(/^\d+px$/)
      expect(property(slot.element, '--node-y')).toMatch(/^\d+px$/)
    }
  })

  it.each([
    ['active', 'ACTIVE'],
    ['starting', 'STARTING'],
    ['degraded', 'DEGRADED'],
  ] as const)('shows the %s runtime status as the shared word', (runtime, label) => {
    const snapshot = structuredClone(topology)
    snapshot.state.runtime = runtime

    const wrapper = mount(TopologyView, { props: { topology: snapshot } })
    const status = wrapper.get('.ox-topology-state')

    expect(status.attributes('role')).toBe('status')
    expect(status.text()).toBe(`${label}schema 1`)
  })

  it('renders every node as a button with the visible text as its only name, typed connectors and symbols', () => {
    const wrapper = mount(TopologyView, { props: { topology } })
    const nodes = wrapper.findAll('.ox-node')

    expect(nodes).toHaveLength(topology.nodes.length)
    expect(nodes.every((node) => node.element.tagName === 'BUTTON' && node.attributes('aria-label') === undefined)).toBe(true)
    expect(nodes.every((node) => node.attributes('aria-pressed') === 'false')).toBe(true)
    expect(wrapper.findAll('.ox-connector')).toHaveLength(topology.edges.length)
    expect(wrapper.findAll('.ox-connector-tls')).toHaveLength(2)
    expect(wrapper.findAll('.ox-node-icon').every((icon) => icon.attributes('aria-hidden') === 'true')).toBe(true)
    expect(wrapper.findAll('.ox-relation')).toHaveLength(topology.edges.length)
    expect(wrapper.get('.ox-relations').text()).toContain('Dispatches to service')
    expect(wrapper.text()).toContain('Socket / 0.0.0.0:443')
    expect(wrapper.text()).toContain('DNS / backend.example.test:3000')
    expect(wrapper.text()).toContain('7 active connections')
    expect(wrapper.text()).toContain('3 active connections')
    expect(wrapper.get('[data-node-id="upstream_pool:3:web"]').text()).toContain('DEGRADED')
    expect(wrapper.get('[data-node-id="endpoint:3:web:14:127.0.0.1:3000"]').text()).toContain('UP')
    expect(wrapper.find('.ox-inspector').exists()).toBe(false)
  })

  it('never cuts node text: long names stay complete in the node', () => {
    const long = structuredClone(topology)
    long.nodes[0]!.name = 'a-very-long-listener-name-that-must-wrap-instead-of-being-truncated'
    const wrapper = mount(TopologyView, { props: { topology: long } })
    expect(wrapper.get('[data-node-id="listener:4:edge"]').text()).toContain(long.nodes[0]!.name)
  })

  it('docks a redacted inspector beside the graph, as definition lists with readable labels', async () => {
    const wrapper = mount(TopologyView, { props: { topology }, attachTo: document.body })
    await wrapper.get('[data-node-id="certificate:6:public"]').trigger('click')

    const inspector = wrapper.get('.ox-inspector')
    expect(wrapper.get('[data-node-id="certificate:6:public"]').attributes('aria-pressed')).toBe('true')
    expect(inspector.text()).toContain('/certificates/0')
    expect(inspector.text()).toContain('certificate:6:public')
    expect(inspector.text()).toContain('Private key path')
    expect(inspector.text()).toContain('<redacted>')
    expect(inspector.text()).toContain('/etc/oxiroute/public.pem')
    expect(inspector.text()).toContain('DNS names')
    expect(inspector.text()).not.toContain('privateKeyPath')
    expect(inspector.text()).not.toContain('{')
    expect(inspector.find('pre').exists()).toBe(false)
    expect(inspector.findAll('dl').length).toBeGreaterThan(1)
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled()
    wrapper.unmount()
  })

  it('shows limits, runtime metrics and identity of the selected node', async () => {
    const wrapper = mount(TopologyView, { props: { topology }, attachTo: document.body })

    await wrapper.get('[data-node-id="rtmp_listener:4:live"]').trigger('click')
    expect(wrapper.get('.ox-inspector').text()).toContain('Manual recorder count')
    expect(wrapper.get('.ox-inspector').text()).toContain('1 KiB')

    await wrapper.get('[data-node-id="listener:4:edge"]').trigger('click')
    expect(wrapper.get('.ox-inspector').text()).toContain('Connection limit')
    expect(wrapper.get('.ox-inspector').text()).toContain('Unbounded')
    expect(wrapper.get('.ox-inspector').text()).toContain('Active connections')

    await wrapper.get('[data-node-id="endpoint:3:web:14:127.0.0.1:3000"]').trigger('click')
    expect(wrapper.get('.ox-inspector').text()).toContain('DNS / backend.example.test:3000')

    await wrapper.get('[data-node-id="http_service:3:web"]').trigger('click')
    expect(wrapper.get('.ox-inspector').text()).toContain('Request body limit')
    expect(wrapper.get('.ox-inspector').text()).toContain('30 s')
    wrapper.unmount()
  })

  it('keeps redacted paths out of the inspector', async () => {
    const wrapper = mount(TopologyView, { props: { topology }, attachTo: document.body })
    await wrapper.get('[data-node-id="http_route:3:web:0::1:/:0:"]').trigger('click')
    const text = wrapper.get('.ox-inspector').text()
    for (const secret of ['tokenFilePath', 'Token file path', '/run/private/token', 'rootDirectory', 'Root directory', '/srv/private-www']) {
      expect(text).not.toContain(secret)
    }
    expect(text).toContain('Spa fallback')
    wrapper.unmount()
  })

  it('moves focus with the arrow, Home and End keys, inspects through the relations, and closes with Escape', async () => {
    const wrapper = mount(TopologyView, { props: { topology }, attachTo: document.body })
    const nodes = wrapper.findAll('.ox-node')
    ;(nodes[0]!.element as HTMLButtonElement).focus()

    await nodes[0]!.trigger('keydown', { key: 'ArrowRight' })
    expect(document.activeElement).toBe(nodes[1]!.element)
    await nodes[1]!.trigger('keydown', { key: 'End' })
    expect(document.activeElement).toBe(nodes.at(-1)!.element)
    await nodes.at(-1)!.trigger('keydown', { key: 'ArrowDown' })
    expect(document.activeElement).toBe(nodes[0]!.element)
    await nodes[0]!.trigger('keydown', { key: 'ArrowLeft' })
    expect(document.activeElement).toBe(nodes.at(-1)!.element)
    await nodes[0]!.trigger('keydown', { key: 'Home' })
    expect(document.activeElement).toBe(nodes[0]!.element)

    await wrapper.findAll('.ox-relation')[0]!.trigger('click')
    expect(wrapper.get('.ox-inspector').text()).toContain('web')

    await wrapper.get('[data-inspector-close]').trigger('click')
    expect(wrapper.find('.ox-inspector').exists()).toBe(false)
    expect(document.activeElement).toBe(wrapper.get('[data-node-id="http_service:3:web"]').element)

    await wrapper.get('[data-node-id="listener:4:edge"]').trigger('click')
    await wrapper.get('[data-section="topology"]').trigger('keydown', { key: 'Escape' })
    expect(wrapper.find('.ox-inspector').exists()).toBe(false)
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
