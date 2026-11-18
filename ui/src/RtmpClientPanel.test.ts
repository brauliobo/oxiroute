import { describe, expect, it } from 'vitest'

import RtmpClientPanel from './RtmpClientPanel.vue'
import type { RtmpClientSnapshot } from './api'
import { contractRtmpStats } from './test/contractFixtures'
import { mount } from './test/mount'

const client = (overrides: Partial<RtmpClientSnapshot> = {}): RtmpClientSnapshot => ({
  id: 'client-1',
  service: 'edge',
  application: 'live',
  stream: 'camera',
  role: 'subscriber',
  peerIp: '192.0.2.10',
  connectedAtUnixMs: 1_750_000_000_000,
  revision: '4',
  ...overrides,
})

describe('RtmpClientPanel', () => {
  it('shows the empty state while no client is connected, and nothing about a snapshot before the first one', () => {
    const none = mount(RtmpClientPanel, { props: { stats: null, busySessionId: null } })
    expect(none.text()).toContain('No connected RTMP clients')
    expect(none.text()).not.toContain('Snapshot')

    const stats = contractRtmpStats()
    const empty = mount(RtmpClientPanel, { props: { stats, busySessionId: null } })
    expect(empty.text()).toContain('Snapshot: 2025-06-15 15:06:40 UTC')
    expect(empty.find('.ox-card').exists()).toBe(false)
  })

  it('never formats the zero timestamp of statistics that were not sampled yet', () => {
    const stats = contractRtmpStats()
    stats.asOfUnixMs = 0
    const wrapper = mount(RtmpClientPanel, { props: { stats, busySessionId: null } })

    expect(wrapper.text()).toContain('Snapshot: no data yet')
    expect(wrapper.text()).not.toContain('1970')
  })

  it('emits the drop of a client or of its role, and disables both while busy', async () => {
    const stats = contractRtmpStats()
    stats.clients = [client()]
    const wrapper = mount(RtmpClientPanel, { props: { stats, busySessionId: null } })

    expect(wrapper.get('.ox-badge').text()).toBe('SUBSCRIBER')
    expect(wrapper.text()).toContain('Stream camera / 192.0.2.10 / Revision 4')
    const [dropClient, dropRole] = wrapper.findAll('button')
    expect(dropClient!.text()).toBe('Drop client')
    expect(dropRole!.text()).toBe('Drop subscriber')
    await dropClient!.trigger('click')
    await dropRole!.trigger('click')
    expect(wrapper.emitted('drop')).toEqual([[stats.clients[0], 'client'], [stats.clients[0], 'subscriber']])

    await wrapper.setProps({ busySessionId: 'client-1' })
    expect(wrapper.findAll('button').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
    expect(wrapper.findAll('button')[0]!.text()).toBe('Dropping...')
  })

  it('names an unbound session and notes a truncated list', () => {
    const stats = contractRtmpStats()
    stats.clients = [client({ application: null, stream: null, peerIp: null, role: 'client' })]
    stats.clientsTruncated = true
    const wrapper = mount(RtmpClientPanel, { props: { stats, busySessionId: null } })

    expect(wrapper.get('h3').text()).toBe('Unbound session')
    expect(wrapper.text()).toContain('Showing the first 1,024 clients.')
    expect(wrapper.findAll('button')).toHaveLength(1)
  })
})
