import { describe, expect, it } from 'vitest'
import { decodeSnapshot } from './decode'
import { deriveModel } from './derive'
import { wireDocument, wirePool, wireServer, wireListener } from './fixtures'
import type { StatsDocument } from './generated/statsPage'
import { buildModel } from './model'
import { SORT_COLUMNS, queryListeners, queryPools } from './query'
import { defaultView } from './urlState'
import type { ViewState } from './urlState'

const measured = (overrides: Partial<StatsDocument> = {}, rates: Record<string, number> = {}) =>
  deriveModel(buildModel(decodeSnapshot(wireDocument(overrides))), new Map(Object.entries(rates)))
const view = (patch: Partial<ViewState> = {}): ViewState => ({ ...defaultView(), ...patch })
const names = (pools: ReturnType<typeof queryPools>) => pools.map((pool) => [pool.name, pool.servers.map((server) => server.name)])

describe('queryPools filter', () => {
  it('keeps everything for the default view', () => {
    expect(names(queryPools(measured().pools, view()))).toEqual([['web', ['origin-a', 'origin-b']], ['api', ['origin-c']]])
  })

  it('matches the text, case-insensitively, against server, pool, address and status words', () => {
    const pools = measured().pools
    expect(names(queryPools(pools, view({ search: 'ORIGIN-B' })))).toEqual([['web', ['origin-b']]])
    expect(names(queryPools(pools, view({ search: 'api' })))).toEqual([['api', ['origin-c']]])
    expect(names(queryPools(pools, view({ search: '198.51.100.12' })))).toEqual([['web', ['origin-b']]])
    expect(names(queryPools(pools, view({ search: 'drain' })))).toEqual([['web', ['origin-b']]])
  })

  it('needs every word of the text to match the same server', () => {
    const pools = measured().pools
    expect(names(queryPools(pools, view({ search: 'web drain' })))).toEqual([['web', ['origin-b']]])
    expect(names(queryPools(pools, view({ search: 'api drain' })))).toEqual([])
    expect(names(queryPools(pools, view({ search: '   ' })))).toHaveLength(2)
  })

  it('treats the text literally, not as a pattern', () => {
    expect(queryPools(measured().pools, view({ search: '.*' }))).toEqual([])
  })

  it('filters by server status and by problems only', () => {
    const pools = measured().pools
    expect(names(queryPools(pools, view({ states: ['drain', 'down'] })))).toEqual([['web', ['origin-b']]])
    expect(names(queryPools(pools, view({ problemsOnly: true })))).toEqual([['web', ['origin-b']]])
    expect(names(queryPools(pools, view({ states: ['up'], problemsOnly: true })))).toEqual([])
  })

  it('keeps a pool without servers unless a server-level filter is active', () => {
    const pools = measured({ pools: [wirePool('empty', [], { status: 'empty' }), wirePool('web', [wireServer('o', '198.51.100.1:1')])] }).pools
    expect(pools.map((pool) => pool.name)).toEqual(['empty', 'web'])
    expect(queryPools(pools, view()).map((pool) => pool.name)).toEqual(['empty', 'web'])
    expect(queryPools(pools, view({ search: 'empty' })).map((pool) => pool.name)).toEqual(['empty'])
    expect(queryPools(pools, view({ states: ['up'] })).map((pool) => pool.name)).toEqual(['web'])
  })

  it('does not change what it is given', () => {
    const pools = measured().pools
    queryPools(pools, view({ search: 'origin-b', sort: { column: 'name', direction: 'desc' } }))
    expect(pools[0]!.servers).toHaveLength(2)
    expect(pools.map((pool) => pool.name)).toEqual(['web', 'api'])
  })
})

describe('queryPools sort', () => {
  const rates = { 'server:web/origin-a#requests': 5, 'server:web/origin-b#requests': 50, 'pool:web#requests': 55,
    'pool:api#requests': 1 }

  it('sorts pools and the servers inside them by the column', () => {
    const pools = measured({}, rates).pools
    expect(names(queryPools(pools, view({ sort: { column: 'name', direction: 'asc' } })))).toEqual(
      [['api', ['origin-c']], ['web', ['origin-a', 'origin-b']]])
    expect(names(queryPools(pools, view({ sort: { column: 'rate', direction: 'desc' } })))).toEqual(
      [['web', ['origin-b', 'origin-a']], ['api', ['origin-c']]])
  })

  it('puts unknown values last in both directions', () => {
    const pools = measured({}, { 'server:web/origin-a#requests': 5 }).pools[0]!.servers
    const sorted = (direction: 'asc' | 'desc') => queryPools([{ ...measured().pools[0]!, servers: pools }], view({ sort: { column: 'rate', direction } }))[0]!.servers.map((s) => s.name)
    expect(sorted('asc')).toEqual(['origin-a', 'origin-b'])
    expect(sorted('desc')).toEqual(['origin-a', 'origin-b'])
  })

  it('sorts by status severity, worst first when ascending', () => {
    const wire = { pools: [wirePool('web', [wireServer('a', '198.51.100.1:1'), wireServer('b', '198.51.100.2:1', { status: 'down', available: false }),
      wireServer('c', '198.51.100.3:1', { status: 'ejected', available: false })])] }
    const sorted = queryPools(measured(wire).pools, view({ sort: { column: 'status', direction: 'asc' } }))[0]!.servers
    expect(sorted.map((server) => server.name)).toEqual(['b', 'c', 'a'])
  })

  it('orders names naturally: case-insensitive, numbers by value', () => {
    const wire = { pools: [wirePool('web', ['b10', 'b9', 'B1', 'a'].map((name) => wireServer(name, '198.51.100.1:1')))] }
    const sorted = queryPools(measured(wire).pools, view({ sort: { column: 'name', direction: 'asc' } }))[0]!.servers
    expect(sorted.map((server) => server.name)).toEqual(['a', 'B1', 'b9', 'b10'])
  })

  it('ignores a sort column it does not know and keeps the server order', () => {
    expect(names(queryPools(measured().pools, view({ sort: { column: 'nonsense', direction: 'asc' } })))[0]).toEqual(['web', ['origin-a', 'origin-b']])
  })

  it('offers the documented columns', () => {
    expect(SORT_COLUMNS).toEqual(['name', 'status', 'active', 'rate', 'errors', 'p99', 'received', 'sent'])
  })
})

describe('queryListeners', () => {
  const listeners = () => measured({ listeners: [wireListener('public-http'), wireListener('admin-tcp', { protocol: 'tcp', state: 'failed', service: 'relay' })] }).listeners

  it('filters by text over name, protocol, service, bind and state, and by problems', () => {
    expect(queryListeners(listeners(), view({ search: 'tcp' })).map((row) => row.name)).toEqual(['admin-tcp'])
    expect(queryListeners(listeners(), view({ search: 'failed' })).map((row) => row.name)).toEqual(['admin-tcp'])
    expect(queryListeners(listeners(), view({ search: '192.0.2.10' }))).toHaveLength(2)
    expect(queryListeners(listeners(), view({ problemsOnly: true })).map((row) => row.name)).toEqual(['admin-tcp'])
  })

  it('does not apply the server status chips to listeners', () => {
    expect(queryListeners(listeners(), view({ states: ['down'] }))).toHaveLength(2)
  })

  it('sorts by name and by problem first', () => {
    expect(queryListeners(listeners(), view({ sort: { column: 'name', direction: 'asc' } })).map((row) => row.name)).toEqual(['admin-tcp', 'public-http'])
    expect(queryListeners(listeners(), view({ sort: { column: 'status', direction: 'asc' } })).map((row) => row.name)).toEqual(['admin-tcp', 'public-http'])
  })
})
