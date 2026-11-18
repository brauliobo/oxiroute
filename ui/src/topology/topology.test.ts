import { describe, expect, it } from 'vitest'

import type { TopologyNode } from '../api'
import { contractTopology } from '../test/contractFixtures'
import { describeAttributes, suppressSensitivePaths } from './attributes'
import { nextFocusIndex } from './focus'
import { activeConnections, configuredLimit, identityLabel } from './identity'
import { NODE_HEIGHT, NODE_WIDTH, layoutTopology } from './layout'

const topology = contractTopology()
const byKind = (kind: TopologyNode['kind']) => topology.nodes.find((node) => node.kind === kind)!

describe('describeAttributes', () => {
  it('turns keys into readable labels and values into readable text', () => {
    const items = describeAttributes({
      minVersion: '1.2',
      upstreamIoTimeoutMs: 30_000,
      maxRequestBodyBytes: 1_572_864,
      port: 8443,
      enabled: true,
      host: null,
      dnsNames: ['example.test', 'www.example.test'],
      noNames: [],
      lastCheckedAtUnixMs: 1_750_000_000_000,
      lastTransitionAtUnixMs: 0,
      acceptedConnections: '12345',
    })

    expect(Object.fromEntries(items.map(({ label, value }) => [label, value]))).toEqual({
      'Min version': '1.2',
      'Upstream IO timeout': '30 s',
      'Max request body bytes': '1.5 MiB',
      Port: '8443',
      Enabled: 'Yes',
      Host: 'None',
      'DNS names': 'example.test, www.example.test',
      'No names': 'None',
      'Last checked at': '2025-06-15 15:06:40 UTC',
      'Last transition at': 'None',
      'Accepted connections': '12,345',
    })
  })

  it('flattens nested objects and lists of objects into one level with a path label', () => {
    const items = describeAttributes({
      bind: { type: 'socket', address: '0.0.0.0:443' },
      applications: [{ name: 'live', recording: { recorderCount: 2 } }, { name: 'vod' }],
    })

    expect(items).toEqual([
      { label: 'Bind / Type', value: 'socket' },
      { label: 'Bind / Address', value: '0.0.0.0:443' },
      { label: 'Applications / 1 / Name', value: 'live' },
      { label: 'Applications / 1 / Recording / Recorder count', value: '2' },
      { label: 'Applications / 2 / Name', value: 'vod' },
    ])
  })

  it('drops the keys that must never be shown, at any depth', () => {
    const attributes = { action: { rootDirectory: '/srv/x', spaFallback: true }, tokenFilePath: '/run/t', token_file_path: '/run/t' }

    expect(suppressSensitivePaths(attributes)).toEqual({ action: { spaFallback: true } })
    expect(describeAttributes(attributes)).toEqual([{ label: 'Action / Spa fallback', value: 'Yes' }])
    expect(JSON.stringify(describeAttributes({ list: [{ root_directory: '/srv/x', ok: 1 }] }))).not.toContain('/srv/x')
  })

  it('describes nothing for an empty object', () => {
    expect(describeAttributes({})).toEqual([])
  })
})

describe('layoutTopology', () => {
  const layout = layoutTopology(topology.nodes, topology.edges)

  it('places each kind in its own column, left to right along the request path', () => {
    const x = (kind: TopologyNode['kind']) => layout.positions.get(byKind(kind).id)!.x
    expect(x('listener')).toBeLessThan(x('http_service'))
    expect(x('http_service')).toBeLessThan(x('http_route'))
    expect(x('http_route')).toBeLessThan(x('upstream_pool'))
    expect(x('upstream_pool')).toBeLessThan(x('endpoint'))
    expect(x('listener')).toBe(x('forward_proxy_listener'))
  })

  it('stacks nodes of one column without overlap and keeps source order', () => {
    const first = layout.positions.get(topology.nodes[0]!.id)!
    const second = layout.positions.get(topology.nodes[1]!.id)!
    expect(second.y - first.y).toBeGreaterThanOrEqual(NODE_HEIGHT)
    expect(layout.orderedNodes.slice(0, 2).map((node) => node.id)).toEqual([topology.nodes[0]!.id, topology.nodes[1]!.id])
  })

  it('sizes the canvas to hold every node and draws one connector per edge between known nodes', () => {
    for (const { x, y } of layout.positions.values()) {
      expect(x + NODE_WIDTH).toBeLessThanOrEqual(layout.width)
      expect(y + NODE_HEIGHT).toBeLessThanOrEqual(layout.height)
    }
    expect(layout.connectors).toHaveLength(topology.edges.length)
    expect(layout.connectors.every(({ path }) => path.startsWith('M '))).toBe(true)
    const dangling = layoutTopology(topology.nodes, [{ ...topology.edges[0]!, target: 'missing' }])
    expect(dangling.connectors).toEqual([])
  })

  it('marks only the TLS relations', () => {
    const tls = layoutTopology(topology.nodes, [
      { id: 'a', kind: 'listener_tls', source: topology.nodes[0]!.id, target: topology.nodes[2]!.id, configPath: '/a' },
      { id: 'b', kind: 'dispatch_service', source: topology.nodes[0]!.id, target: topology.nodes[2]!.id, configPath: '/b' },
    ])
    expect(tls.connectors.map(({ tls: secure }) => secure)).toEqual([true, false])
  })

  it('lays out an empty topology with a usable canvas', () => {
    const empty = layoutTopology([], [])
    expect(empty.height).toBeGreaterThan(0)
    expect(empty.connectors).toEqual([])
  })
})

describe('nextFocusIndex', () => {
  it.each([
    ['ArrowRight', 1, 4, 2],
    ['ArrowDown', 3, 4, 0],
    ['ArrowLeft', 0, 4, 3],
    ['ArrowUp', 2, 4, 1],
    ['Home', 2, 4, 0],
    ['End', 1, 4, 3],
  ])('%s from %i of %i goes to %i', (key, current, count, expected) => {
    expect(nextFocusIndex(key, current, count)).toBe(expected)
  })

  it('ignores other keys and focus outside the nodes', () => {
    expect(nextFocusIndex('Enter', 1, 4)).toBeNull()
    expect(nextFocusIndex('ArrowRight', -1, 4)).toBeNull()
    expect(nextFocusIndex('ArrowRight', 0, 0)).toBeNull()
  })
})

describe('node identity', () => {
  it('names the bind of a listener and the address of an endpoint', () => {
    expect(identityLabel(byKind('listener'))).toBe('Socket / 127.0.0.1:8080')
    expect(identityLabel(byKind('forward_proxy_listener'))).toBe('UDP / 0.0.0.0:8443')
    expect(identityLabel(byKind('endpoint'))).toBe('Socket / 127.0.0.1:3000')
    expect(identityLabel(byKind('http_service'))).toBe('')
    expect(identityLabel({ ...byKind('listener'), attributes: { bind: { type: 'unix', path: '/run/x.sock', mode: null } } })).toBe('Unix / /run/x.sock')
    expect(identityLabel({ ...byKind('endpoint'), attributes: { type: 'dns', host: 'origin.example.com', port: 80 } })).toBe('DNS / origin.example.com:80')
    expect(identityLabel({ ...byKind('endpoint'), attributes: { type: 'unix', path: '/run/o.sock' } })).toBe('Unix / /run/o.sock')
    expect(identityLabel({ ...byKind('endpoint'), attributes: { type: 'dns' } })).toBe('')
  })

  it('reads configured limits in the unit of the limit', () => {
    expect(configuredLimit(byKind('listener'))).toEqual({ label: 'Connection limit', value: '1,000' })
    expect(configuredLimit({ ...byKind('listener'), attributes: { maxConnections: null } })).toEqual({ label: 'Connection limit', value: 'Unbounded' })
    expect(configuredLimit(byKind('http_service'))).toEqual({ label: 'Request body limit', value: 'Unbounded' })
    expect(configuredLimit({ ...byKind('http_service'), attributes: { maxRequestBodyBytes: 1_048_576 } })).toEqual({ label: 'Request body limit', value: '1 MiB' })
    expect(configuredLimit(byKind('endpoint'))).toBeNull()
  })

  it('accepts only exact non-negative integer connection counts', () => {
    const overlay = (value: unknown) => ({ nodeId: 'x', state: 'listening' as const, metrics: { activeConnections: value as number } })
    expect(activeConnections(overlay(7))).toBe('7')
    expect(activeConnections(overlay('18446744073709551615'))).toBe('18446744073709551615')
    expect(activeConnections(overlay(-1))).toBeNull()
    expect(activeConnections(overlay(1.5))).toBeNull()
    expect(activeConnections(overlay('01'))).toBeNull()
    expect(activeConnections(undefined)).toBeNull()
  })
})
