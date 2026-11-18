import { describe, expect, it } from 'vitest'

import { mount } from '../test/mount'
import { contractCatalog, contractMonitoring } from '../test/contractFixtures'
import ListenerTable from './ListenerTable.vue'
import MetricList from './MetricList.vue'
import MonitoringOverview from './MonitoringOverview.vue'
import PoolCard from './PoolCard.vue'
import ServerTable from './ServerTable.vue'
import StreamInventory from './StreamInventory.vue'
import {
  formatDecimal, formatLimit, formatOptionalBytes, formatOptionalCount, formatOptionalPercent, formatSampleAge,
  memoryUsagePercent, totalTrafficBytes, usedMemoryBytes,
} from './telemetry'

const monitoring = contractMonitoring()
const sampledAt = monitoring.sampledAtUnixMs

describe('MetricList', () => {
  it('renders label and value pairs as a definition list, monospace and titled on request', () => {
    const wrapper = mount(MetricList, {
      props: { large: true, items: [{ label: 'Stable ID', value: 'a:b', mono: true, title: 'exact' }, { label: 'Threads', value: '8' }] },
    })

    expect(wrapper.element.tagName).toBe('DL')
    expect(wrapper.classes()).toContain('ox-metrics-large')
    expect(wrapper.findAll('dt').map((term) => term.text())).toEqual(['Stable ID', 'Threads'])
    expect(wrapper.findAll('dd').map((value) => value.text())).toEqual(['a:b', '8'])
    expect(wrapper.findAll('dd')[0]!.classes()).toContain('ox-mono')
    expect(wrapper.findAll('dd')[0]!.attributes('title')).toBe('exact')
    expect(wrapper.findAll('dd')[1]!.classes()).not.toContain('ox-mono')
  })
})

describe('ListenerTable', () => {
  it('shows one row per listener with its state word, limits and IEC byte counts', () => {
    const wrapper = mount(ListenerTable, { props: { listeners: monitoring.listeners } })

    expect(wrapper.findAll('thead th').map((cell) => cell.text())).toEqual([
      'Listener', 'State', 'Connections', 'Accepted', 'Rejected', 'Received', 'Sent',
    ])
    const rows = wrapper.findAll('tbody')
    expect(rows).toHaveLength(3)
    expect(rows[0]!.text()).toContain('HTTP / socket:127.0.0.1:8080')
    expect(rows[0]!.text()).toContain('14 / 1,000')
    expect(rows[0]!.text()).toContain('1 MiB')
    expect(rows[1]!.text()).toContain('28 / Unbounded')
    expect(rows[1]!.text()).toContain('Maintenance by operator')
    expect(rows[0]!.text()).not.toContain('by operator')
    expect(rows.map((row) => row.get('.ox-badge').text())).toEqual(['LISTENING', 'FAILED', 'STOPPED'])
    expect(rows.every((row) => row.find('tr > td[data-label="State"]').exists())).toBe(true)
  })
})

describe('ServerTable', () => {
  const pool = monitoring.upstreamPools[0]!

  it('derives each status from administration, ejection and health, with the last failure beside it', () => {
    const wrapper = mount(ServerTable, {
      props: { poolName: pool.name, servers: pool.endpoints, sampledAtUnixMs: sampledAt },
    })

    expect(wrapper.get('caption').text()).toBe('web-backends servers')
    expect(wrapper.findAll('tbody').map((row) => row.get('.ox-badge').text())).toEqual(['UP', 'DRAIN'])
    expect(wrapper.text()).toContain('Last failure: Connection failed')
    expect(wrapper.text()).toContain('3 / 100')
    expect(wrapper.text()).toContain('Checked just now')
    expect(wrapper.text()).not.toContain('Override')
  })

  it('shows an ejected server as EJECTED and forced health as the override', () => {
    const ejected = structuredClone(pool.endpoints)
    ejected[0]!.passiveEjected = true
    ejected[1]!.healthOverride = 'down'
    ejected[1]!.administrativeState = 'ready'
    const wrapper = mount(ServerTable, {
      props: { poolName: pool.name, servers: ejected, sampledAtUnixMs: sampledAt, detailed: true },
    })

    expect(wrapper.findAll('tbody').map((row) => row.get('.ox-badge').text())).toEqual(['EJECTED', 'DOWN'])
    expect(wrapper.text()).toContain('Forced down')
    expect(wrapper.findAll('thead th').map((cell) => cell.text())).toContain('Administration')
  })

  it('says when no check or transition happened yet', () => {
    const unchecked = structuredClone(pool.endpoints)
    unchecked[0]!.lastCheckedAtUnixMs = null
    unchecked[0]!.lastTransitionAtUnixMs = null
    unchecked[0]!.state = 'unchecked'
    const wrapper = mount(ServerTable, {
      props: { poolName: pool.name, servers: unchecked.slice(0, 1), sampledAtUnixMs: sampledAt },
    })

    expect(wrapper.text()).toContain('No checks completed')
    expect(wrapper.text()).toContain('No transition yet')
    expect(wrapper.get('.ox-badge').text()).toBe('UNCHECKED')
  })
})

describe('PoolCard', () => {
  it('reads availability as the shared pool words and pluralizes servers', () => {
    const pool = structuredClone(monitoring.upstreamPools[0]!)
    const degraded = mount(PoolCard, { props: { pool, sampledAtUnixMs: sampledAt } })
    expect(degraded.get('.ox-card-head .ox-badge').text()).toBe('DEGRADED')
    expect(degraded.text()).toContain('1 of 2 servers available')
    expect(degraded.text()).toContain('Unavailable selections')
    expect(degraded.text()).not.toContain('Queued total')

    pool.endpoints = pool.endpoints.slice(0, 1)
    pool.totalEndpoints = 1
    const up = mount(PoolCard, { props: { pool, sampledAtUnixMs: sampledAt, detailed: true } })
    expect(up.get('.ox-card-head .ox-badge').text()).toBe('UP')
    expect(up.text()).toContain('1 of 1 server available')
    expect(up.text()).toContain('Queued total')

    pool.endpoints = []
    pool.totalEndpoints = 0
    pool.availableEndpoints = 0
    const empty = mount(PoolCard, { props: { pool, sampledAtUnixMs: sampledAt } })
    expect(empty.get('.ox-card-head .ox-badge').text()).toBe('EMPTY')
    expect(empty.text()).toContain('This pool has no servers.')
    expect(empty.find('table').exists()).toBe(false)
  })
})

describe('MonitoringOverview', () => {
  it('draws load and memory as meters whose fill comes from custom properties', () => {
    const wrapper = mount(MonitoringOverview, { props: { monitoring, currentUnixMs: sampledAt + 3000 } })
    const fills = wrapper.findAll('.ox-meter-fill').map((fill) => (fill.element as HTMLElement).style.getPropertyValue('--ox-w'))

    expect(fills).toEqual(['42', '31', '25', '75'])
    expect(wrapper.findAll('[role="meter"]').map((meter) => meter.attributes('aria-label'))).toEqual([
      'Load average 1 min', 'Load average 5 min', 'Load average 15 min', 'Host memory used',
    ])
    expect(wrapper.text()).toContain('3 s ago')
    expect(wrapper.text()).toMatch(/Snapshot \d{4}-\d\d-\d\d \d\d:\d\d:\d\d UTC/)
    expect(wrapper.text()).toContain('3 listeners')
    expect(wrapper.text()).toContain('1 pool')
    expect(wrapper.text()).toContain('2 GiB')
    expect(wrapper.text()).not.toMatch(/\b(KB|MB|GB)\b/)
  })

  it('shows an unavailable load as such, with an empty meter', () => {
    const sample = structuredClone(monitoring)
    sample.host.loadAverage5m = null
    const wrapper = mount(MonitoringOverview, { props: { monitoring: sample, currentUnixMs: sampledAt } })

    expect(wrapper.findAll('[role="meter"]')[1]!.attributes('aria-valuetext')).toBe('Unavailable')
    expect(wrapper.findAll('[role="meter"]')[1]!.attributes('aria-valuenow')).toBe('0')
  })
})

describe('StreamInventory', () => {
  const props = { currentUnixMs: 1_750_000_060_000, busyRecorderId: null, visible: true }

  it('shows the streams, their tracks and relays the control event of a recorder', async () => {
    const catalog = contractCatalog()
    const wrapper = mount(StreamInventory, { props: { ...props, catalog } })

    expect(wrapper.get('h3').text()).toBe('live / camera')
    expect(wrapper.text()).toContain('PUBLISHING')
    expect(wrapper.text()).toContain('12 viewers')
    expect(wrapper.text()).toContain('1m 0s')
    expect(wrapper.text()).toContain('AAC / 1 KiB received / RTMP 120 ms')
    expect(wrapper.text()).toContain('Snapshot: 2025-06-15 15:06:40 UTC')
    await wrapper.get('[data-recorder-action]').trigger('click')
    expect(wrapper.emitted('control')?.[0]?.[0]).toEqual(expect.objectContaining({ id: catalog.streams[0]!.id }))
  })

  it('pluralizes a single viewer and marks a stream without publisher as waiting', () => {
    const catalog = contractCatalog()
    catalog.streams[0]!.subscriber_count = 1
    catalog.streams[0]!.publisher = null
    const wrapper = mount(StreamInventory, { props: { ...props, catalog } })

    expect(wrapper.text()).toContain('1 viewer')
    expect(wrapper.text()).not.toContain('1 viewers')
    expect(wrapper.get('.ox-card-head .ox-badge').text()).toBe('WAITING')
  })

  it('never formats the zero timestamp of a catalog that was not sampled yet', () => {
    const catalog = contractCatalog()
    catalog.streams = []
    catalog.as_of_unix_ms = 0
    const wrapper = mount(StreamInventory, { props: { ...props, catalog } })

    expect(wrapper.text()).toContain('Snapshot: no data yet')
    expect(wrapper.text()).not.toContain('1970')
    expect(wrapper.text()).toContain('No active stream sessions')
  })

  it('renders nothing before the first catalog', () => {
    const wrapper = mount(StreamInventory, { props: { ...props, catalog: null } })
    expect(wrapper.find('.ox-card').exists()).toBe(false)
    expect(wrapper.find('.ox-empty').exists()).toBe(false)
  })
})

describe('telemetry formatting', () => {
  it('formats bytes in IEC units and absent values as unavailable', () => {
    expect(formatOptionalBytes(1_572_864)).toBe('1.5 MiB')
    expect(formatOptionalBytes(null)).toBe('Unavailable')
    expect(formatOptionalCount(null)).toBe('Unavailable')
    expect(formatOptionalCount(12_345)).toBe('12,345')
    expect(formatDecimal(0.5)).toBe('0.50')
    expect(formatDecimal(null)).toBe('Unavailable')
    expect(formatLimit(null)).toBe('Unbounded')
    expect(formatOptionalPercent(12.5)).toBe('12.5%')
    expect(formatOptionalPercent(10)).toBe('10%')
  })

  it('derives memory use and total traffic without losing precision', () => {
    expect(usedMemoryBytes(monitoring.host)).toBe(12_884_901_888)
    expect(memoryUsagePercent(monitoring.host)).toBe(75)
    expect(memoryUsagePercent({ ...monitoring.host, totalMemoryBytes: 0, availableMemoryBytes: 0 })).toBe(0)
    expect(memoryUsagePercent({ ...monitoring.host, totalMemoryBytes: null })).toBeNull()
    expect(totalTrafficBytes({ ...monitoring.traffic, bytesReceived: '18446744073709551615', bytesSent: '1' })).toBe('18446744073709551616')
  })

  it('writes the age of a sample with the shared age words', () => {
    expect(formatSampleAge(10_500, 10_000)).toBe('just now')
    expect(formatSampleAge(15_000, 10_000)).toBe('5 s ago')
    expect(formatSampleAge(1_000, 5_000)).toBe('just now')
  })
})
