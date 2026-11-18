import { mount } from './test/mount'
import { describe, expect, it } from 'vitest'

import HaproxyStatsDashboard from './HaproxyStatsDashboard.vue'
import { contractMonitoring } from './test/contractFixtures'

describe('HaproxyStatsDashboard', () => {
  it('renders process, listener, pool and server counters from one snapshot with the shared status words', () => {
    const wrapper = mount(HaproxyStatsDashboard, { props: { monitoring: contractMonitoring() } })

    expect(wrapper.get('h2').text()).toBe('Statistics')
    expect(wrapper.text()).not.toContain('HAProxy')
    expect(wrapper.get('[data-section="statistics"] .ox-metrics-large').findAll('dt')).toHaveLength(4)
    const statistics = wrapper.get('[data-section="statistics"]').text()
    expect(statistics).toContain('10,000')
    expect(statistics).toContain('Retry attempts')
    expect(statistics).toContain('1d 1h')
    expect(statistics).toContain('256 MiB')
    expect(statistics).toMatch(/Snapshot \d{4}-\d\d-\d\d \d\d:\d\d:\d\d UTC/)

    const listeners = wrapper.get('[data-section="listeners"]')
    expect(listeners.text()).toContain('HTTP ingress')
    expect(listeners.text()).toContain('Forward H3')
    expect(listeners.text()).toContain('14 / 1,000')
    expect(listeners.findAll('.ox-badge').map((badge) => badge.text())).toEqual(['LISTENING', 'FAILED', 'STOPPED'])
    expect(listeners.find('table').exists()).toBe(true)

    const pools = wrapper.get('[data-section="pools"]')
    expect(pools.text()).toContain('web-backends')
    expect(pools.text()).toContain('Queued total')
    expect(pools.text()).toContain('primary')
    expect(pools.text()).toContain('127.0.0.1:3000')
    expect(pools.text()).toContain('Last failure: Connection failed')
    expect(pools.findAll('.ox-badge').map((badge) => badge.text())).toEqual(['DEGRADED', 'UP', 'DRAIN'])
    expect(pools.text()).toContain('Administration')
  })

  it('pluralizes counts', () => {
    const monitoring = contractMonitoring()
    monitoring.listeners = monitoring.listeners.slice(0, 1)
    const wrapper = mount(HaproxyStatsDashboard, { props: { monitoring } })

    expect(wrapper.get('[data-section="listeners"]').text()).toContain('1 listener')
    expect(wrapper.get('[data-section="listeners"]').text()).not.toContain('1 listeners')
    expect(wrapper.get('[data-section="pools"]').text()).toContain('1 pool')
  })

  it('renders explicit empty states for absent listeners and upstream pools', () => {
    const monitoring = contractMonitoring()
    monitoring.listeners = []
    monitoring.upstreamPools = []

    const wrapper = mount(HaproxyStatsDashboard, { props: { monitoring } })

    expect(wrapper.text()).toContain('No listeners are currently bound.')
    expect(wrapper.text()).toContain('No upstream pools are configured.')
    expect(wrapper.findAll('.ox-empty')).toHaveLength(2)
    expect(wrapper.find('table').exists()).toBe(false)
  })

  it('renders unavailable telemetry distinctly from numeric zero', () => {
    const unavailable = contractMonitoring()
    unavailable.process.cpuPercent = null
    unavailable.process.residentMemoryBytes = null
    unavailable.process.virtualMemoryBytes = null
    unavailable.process.threadCount = null
    unavailable.process.openFileDescriptors = null
    unavailable.host.loadAverage1m = null
    unavailable.host.loadAverage5m = null
    unavailable.host.loadAverage15m = null
    unavailable.host.totalMemoryBytes = null
    unavailable.host.availableMemoryBytes = null

    const wrapper = mount(HaproxyStatsDashboard, { props: { monitoring: unavailable } })
    const cards = wrapper.findAll('.ox-card')
    expect(cards[0]!.text()).toContain('Unavailable')
    expect(cards[2]!.text()).toContain('Unavailable')

    const zero = contractMonitoring()
    zero.process.cpuPercent = 0
    zero.process.residentMemoryBytes = 0
    zero.process.virtualMemoryBytes = 0
    zero.process.threadCount = 0
    zero.process.openFileDescriptors = 0
    zero.host.loadAverage1m = 0
    zero.host.loadAverage5m = 0
    zero.host.loadAverage15m = 0
    zero.host.totalMemoryBytes = 0
    zero.host.availableMemoryBytes = 0

    const zeroCards = mount(HaproxyStatsDashboard, { props: { monitoring: zero } }).findAll('.ox-card')
    expect(zeroCards[0]!.text()).toContain('0%')
    expect(zeroCards[2]!.text()).toContain('0.00')
    expect(zeroCards[2]!.text()).not.toContain('Unavailable')
  })
})
