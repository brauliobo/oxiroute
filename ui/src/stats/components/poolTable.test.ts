import { describe, expect, it } from 'vitest'
import { nextTick } from 'vue'
import { flushPromises } from '../../test/mount'
import { counter, wirePool, wireServer } from '../fixtures'
import type { StatsDocument } from '../generated/statsPage'
import PoolTable from './PoolTable.vue'
import { documentAt, loadedStore, mountView } from './testing'

const SERVER_ROW = '[data-row^="server:"]'
const names = (wrapper: ReturnType<typeof mountView>) => wrapper.findAll(`${SERVER_ROW} .ox-label`).map((item) => item.text())
const cellOf = (wrapper: ReturnType<typeof mountView>, row: string, label: string) =>
  wrapper.get(`[data-row="${row}"] [data-label="${label}"]`)

// The second sample 2 s later: origin-a served 200 more requests and sent 2000 more bytes, and failed 10 of them.
const grown = (wire: StatsDocument) => {
  const traffic = wire.pools[0]!.servers[0]!.traffic
  traffic.sessions.total = counter(1200)
  traffic.bytesSent = counter(502_000)
  traffic.attempts.connectFailed = counter(20)
}

describe('PoolTable', () => {
  it('is a real table: caption, column headers with scope, a pool head and totals row per pool, a body per server', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0)]))
    expect(wrapper.get('table caption').text()).toContain('Pools with their servers')
    const headers = wrapper.findAll('thead th')
    expect(headers.map((header) => header.attributes('scope'))).toEqual(Array(12).fill('col'))
    expect(headers.map((header) => header.text())).toContain('Latency')
    expect(wrapper.findAll('tbody.ox-pool')).toHaveLength(2)
    expect(wrapper.findAll('tbody.ox-pool tr.ox-total th[scope=row]')).toHaveLength(2)
    expect(names(wrapper)).toEqual(['origin-a', 'origin-b', 'origin-c'])
    expect(wrapper.findAll(`${SERVER_ROW} th[scope=row]`)).toHaveLength(3)
  })

  it('shows the status of every server as a word and a shape', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0)]))
    const status = cellOf(wrapper, 'server:web/origin-b', 'Status')
    expect(status.get('.ox-badge').text()).toBe('DRAIN')
    expect(status.get('svg').attributes('aria-hidden')).toBe('true')
    expect(status.text()).toContain('admin drain')
    expect(cellOf(wrapper, 'pool:web', 'Status').text()).toBe('DEGRADED')
    expect(wrapper.get('[data-row="pool:web"] .ox-group').text()).toContain('1 of 2 servers available')
  })

  it('shows what the snapshot holds and an em dash for what it does not, never a made-up zero', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0, (wire) => {
      const server = wire.pools[0]!.servers[0]!
      delete server.maxConnections
      delete server.downtimeMs
      delete server.check.lastCheckedAtUnixMs
    })]))
    expect(cellOf(wrapper, 'server:web/origin-a', 'Sessions').text()).toContain('3 / —')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Sessions').text()).toContain('1k total')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Downtime').text()).toContain('—')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Check').text()).toContain('checked —')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Req/s').text()).toBe('—')
    expect(cellOf(wrapper, 'server:web/origin-a', 'In').text()).toContain('1.9 MiB')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Latency').text()).toContain('p99 5 s')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Responses').text()).toContain('2xx 900')
  })

  it('shows rates and a sparkline from the second sample on', async () => {
    const harness = await loadedStore([documentAt(0), documentAt(2, grown)])
    const wrapper = mountView(PoolTable, harness)
    expect(wrapper.find(`[data-row="server:web/origin-a"] .ox-spark`).exists()).toBe(false)
    await harness.next()
    await harness.next()
    expect(cellOf(wrapper, 'server:web/origin-a', 'Req/s').text()).toBe('100/s')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Out').text()).toContain('1000 B/s')
    expect(cellOf(wrapper, 'server:web/origin-a', 'Errors').text()).toContain('5/s')
  })

  it('opens and closes the details of a server, keeping the state in the view', async () => {
    const harness = await loadedStore([documentAt(0)])
    const wrapper = mountView(PoolTable, harness)
    const toggle = wrapper.get('[data-row="server:web/origin-a"] .ox-toggle')
    expect(toggle.attributes('aria-expanded')).toBe('false')
    expect(wrapper.find('.ox-details').exists()).toBe(false)
    await toggle.trigger('click')
    expect(toggle.attributes('aria-expanded')).toBe('true')
    expect(toggle.attributes('aria-controls')).toBe(wrapper.get('.ox-details').attributes('id'))
    const details = wrapper.get('.ox-details').text()
    for (const text of ['198.51.100.11:8080', 'Health check', 'Passive health', 'Attempts by outcome', 'Evacuations',
      'Responses by class', 'Request duration', 'Time to first header', 'Not reported by this build']) {
      expect(details).toContain(text)
    }
    expect(harness.store.view.value.expanded).toEqual(['server:web/origin-a'])
    await toggle.trigger('click')
    expect(toggle.attributes('aria-expanded')).toBe('false')
    expect(wrapper.find('.ox-details').exists()).toBe(false)
  })

  it('shows every attempt outcome, passive health value and absent field of a server in its details', async () => {
    const harness = await loadedStore([documentAt(0, (wire) => {
      wire.pools[0]!.servers[0]!.passive = {
        ejected: true, consecutiveFailures: 3, failureCount: counter(7), ejectionCount: counter(2), recoveryCount: counter(1),
        ejectionReason: 'timeout',
      }
    })], '?open=server:web/origin-a')
    const wrapper = mountView(PoolTable, harness)
    const list = (heading: string) => wrapper.findAll('.ox-detail-grid section').find((section) => section.get('h4').text() === heading)!
    expect(list('Passive health').text()).toContain('Ejection reasontimeout')
    expect(list('Passive health').text()).toContain('Ejected until—')
    expect(list('Attempts by outcome').text()).toContain('Connect failed10')
    expect(list('Attempts by outcome').text()).toContain('Status retry10')
  })

  it('folds the servers of a pool away and back', async () => {
    const harness = await loadedStore([documentAt(0)])
    const wrapper = mountView(PoolTable, harness)
    const fold = wrapper.get('[data-row="pool:web"] .ox-group .ox-toggle')
    expect(fold.attributes('aria-expanded')).toBe('true')
    await fold.trigger('click')
    expect(fold.attributes('aria-expanded')).toBe('false')
    expect(names(wrapper)).toEqual(['origin-c'])
    expect(wrapper.get('[data-row="pool:web"] .ox-group').text()).toContain('2 of 2 servers shown')
    expect(harness.store.view.value.collapsed).toEqual(['pool:web'])
    await fold.trigger('click')
    expect(names(wrapper)).toEqual(['origin-a', 'origin-b', 'origin-c'])
  })

  it('sorts by a column header: first the largest, then the smallest, then the source order', async () => {
    const harness = await loadedStore([documentAt(0, (wire) => {
      wire.pools[0]!.servers[0]!.traffic.sessions.current = 1
      wire.pools[0]!.servers[1]!.traffic.sessions.current = 9
    })])
    const wrapper = mountView(PoolTable, harness)
    const header = () => wrapper.findAll('thead th').find((item) => item.text().startsWith('Sessions'))!
    expect(header().attributes('aria-sort')).toBe('none')
    await header().get('button').trigger('click')
    expect(header().attributes('aria-sort')).toBe('descending')
    expect(harness.store.view.value.sort).toEqual({ column: 'active', direction: 'desc' })
    expect(names(wrapper)).toEqual(['origin-b', 'origin-a', 'origin-c'])
    await header().get('button').trigger('click')
    expect(header().attributes('aria-sort')).toBe('ascending')
    expect(names(wrapper)).toEqual(['origin-c', 'origin-a', 'origin-b'])
    await header().get('button').trigger('click')
    expect(header().attributes('aria-sort')).toBe('none')
    expect(harness.store.view.value.sort).toBeUndefined()
    expect(wrapper.findAll('thead th').filter((item) => item.find('button').exists())).toHaveLength(8)
  })

  it('does not sort the columns the store cannot sort by', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0)]))
    const plain = wrapper.findAll('thead th').filter((item) => !item.find('button').exists())
    expect(plain.map((item) => item.text())).toEqual(['Responses', 'Check', 'Weight', 'Downtime'])
    expect(plain.every((item) => item.attributes('aria-sort') === undefined)).toBe(true)
  })

  it('follows the filters of the view and says when nothing matches', async () => {
    const harness = await loadedStore([documentAt(0)])
    const wrapper = mountView(PoolTable, harness)
    harness.store.setView({ search: 'origin-c' })
    await nextTick()
    expect(names(wrapper)).toEqual(['origin-c'])
    harness.store.setView({ search: 'nothing-like-this' })
    await nextTick()
    expect(wrapper.find('table').exists()).toBe(false)
    expect(wrapper.text()).toContain('No pools or servers match the current filters.')
  })

  it('says so when there are no pools', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0, (wire) => { wire.pools = [] })]))
    expect(wrapper.text()).toContain('No pools are configured.')
  })

  it('shows an empty pool without servers', async () => {
    const wrapper = mountView(PoolTable, await loadedStore([documentAt(0, (wire) => { wire.pools = [wirePool('idle', [])] })]))
    expect(wrapper.get('[data-row="pool:idle"] .ox-group').text()).toContain('EMPTY')
    expect(wrapper.findAll(SERVER_ROW)).toHaveLength(0)
  })

  it('opens the details of a pool: selections, queue, canary and latency', async () => {
    const harness = await loadedStore([documentAt(0, (wire) => {
      wire.pools[0]!.queue.byClass = [{ class: 'bulk', count: 4 }]
      wire.pools[0]!.queue.timeoutMs = 5000
    })])
    const wrapper = mountView(PoolTable, harness)
    const more = wrapper.get('[data-row="pool:web"] .ox-more')
    expect(more.attributes('aria-expanded')).toBe('false')
    await more.trigger('click')
    expect(more.attributes('aria-expanded')).toBe('true')
    const text = wrapper.get('.ox-details').text()
    for (const part of ['Algorithmround_robin', 'Selections with no server available0', 'Waiting, class bulk4', 'Wait limit5 s', 'Queue wait',
      'Request duration']) expect(text).toContain(part)
  })

  it('renders names as text, never as markup', async () => {
    const hostile = '<script>alert(1)</script>'
    const harness = await loadedStore([documentAt(0, (wire) => {
      wire.pools = [wirePool(hostile, [wireServer('"><img src=x onerror=alert(1)>', '198.51.100.9:1')])]
    })])
    const wrapper = mountView(PoolTable, harness)
    expect(wrapper.text()).toContain(hostile)
    expect(wrapper.text()).toContain('"><img src=x onerror=alert(1)>')
    expect(wrapper.find('script').exists()).toBe(false)
    expect(wrapper.find('img').exists()).toBe(false)
    await wrapper.get(`${SERVER_ROW} .ox-toggle`).trigger('click')
    expect(wrapper.find('img').exists()).toBe(false)
  })

  it('marks a server whose status changed with text, then clears the mark', async () => {
    const harness = await loadedStore([documentAt(0), documentAt(2, (wire) => {
      wire.pools[0]!.servers[0]!.status = 'down'
    })])
    const wrapper = mountView(PoolTable, harness)
    expect(wrapper.find('.ox-changed').exists()).toBe(false)
    await harness.next()
    await flushPromises()
    const row = wrapper.get('[data-row="server:web/origin-a"]')
    expect(row.classes()).toContain('ox-changed')
    expect(row.get('.ox-changed-tag').text()).toBe('status changed')
    expect(wrapper.findAll('.ox-changed')).toHaveLength(1)
  })
})

