import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import HaproxyStatsDashboard from './HaproxyStatsDashboard.vue'
import { contractMonitoring } from './test/contractFixtures'

describe('HaproxyStatsDashboard', () => {
  it('renders process, listener, backend, and server counters from one snapshot', () => {
    const wrapper = mount(HaproxyStatsDashboard, {
      props: { monitoring: contractMonitoring() },
    })

    expect(wrapper.get('#stats-heading').text()).toBe('Statistics')
    expect(wrapper.findAll('.stats-kpi')).toHaveLength(4)
    expect(wrapper.get('.stats-panel').text()).toContain(new Intl.NumberFormat().format(10_000))
    expect(wrapper.get('.stats-panel').text()).toContain('Retry attempts')
    expect(wrapper.get('.stats-panel').text()).toContain('1d 1h')
    expect(wrapper.get('.stats-section').text()).toContain('HTTP ingress')
    expect(wrapper.get('.stats-section').text()).toContain('Forward H3')
    expect(wrapper.get('.stats-section').text()).toContain(
      `14 / ${new Intl.NumberFormat().format(1_000)}`,
    )

    const poolSection = wrapper.findAll('.stats-section')[1]
    expect(poolSection?.text()).toContain('web-backends')
    expect(poolSection?.text()).toContain('Queued total')
    expect(poolSection?.text()).toContain('7')
    expect(poolSection?.text()).toContain('primary')
    expect(poolSection?.text()).toContain('127.0.0.1:3000')
    expect(poolSection?.text()).toContain('100')
    expect(poolSection?.text()).toContain('Last failure: Connect Failed')
  })

  it('renders explicit empty states for absent listeners and upstream pools', () => {
    const monitoring = contractMonitoring()
    monitoring.listeners = []
    monitoring.upstreamPools = []

    const wrapper = mount(HaproxyStatsDashboard, { props: { monitoring } })

    expect(wrapper.text()).toContain('No listeners are currently bound.')
    expect(wrapper.text()).toContain('No upstream pools are configured.')
    expect(wrapper.findAll('.empty-state')).toHaveLength(2)
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
    expect(wrapper.get('.stats-panel').text()).toContain('Unavailable')
    expect(wrapper.findAll('.stats-panel')[2]!.text()).toContain('Unavailable')

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

    const zeroWrapper = mount(HaproxyStatsDashboard, { props: { monitoring: zero } })
    expect(zeroWrapper.get('.stats-panel').text()).toContain('0%')
    expect(zeroWrapper.findAll('.stats-panel')[2]!.text()).toContain('0.00')
    expect(zeroWrapper.findAll('.stats-panel')[2]!.text()).not.toContain('Unavailable')
  })
})
