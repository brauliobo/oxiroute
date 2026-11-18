import { describe, expect, it } from 'vitest'
import { flushPromises, mount } from '../test/mount'
import { managementStatus } from '../test/managementFixtures'
import { operationsInventory } from '../test/operationsFixtures'
import ForceToggle from './ForceToggle.vue'
import LimitForm from './LimitForm.vue'
import PoolTable from './PoolTable.vue'
import RestartBanner from './RestartBanner.vue'
import ServerCard from './ServerCard.vue'
import StateButtons from './StateButtons.vue'

const inventory = () => operationsInventory()
const entry = (name: string) => inventory().servers.servers.find(({ server }) => server.name === name)!

describe('StateButtons', () => {
  it('marks the current state as pressed and emits the chosen one', async () => {
    const wrapper = mount(StateButtons, { props: { current: 'drain', subject: 'pool web', disabled: false } })

    expect(wrapper.get('[data-state="drain"]').attributes('aria-pressed')).toBe('true')
    expect(wrapper.get('[data-state="ready"]').attributes('aria-pressed')).toBe('false')
    expect(wrapper.get('[data-state="ready"]').attributes('aria-label')).toBe('Ready pool web')
    await wrapper.get('[data-state="maintenance"]').trigger('click')
    expect(wrapper.emitted('change')).toEqual([['maintenance']])
  })

  it('presses nothing for a mixed pool and disables every button', () => {
    const wrapper = mount(StateButtons, { props: { current: null, subject: 'pool web', disabled: true } })
    expect(wrapper.findAll('[aria-pressed="true"]')).toHaveLength(0)
    expect(wrapper.findAll('button').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
  })
})

describe('RestartBanner', () => {
  it('lists the reasons and since when', () => {
    const reasons = [{ code: 'c', path: '/listeners/a', summary: 'needs a restart' }]
    const wrapper = mount(RestartBanner, {
      props: { restart: { required: true, candidateRevision: 'r', since: 0, reasons } },
    })
    expect(wrapper.text()).toContain('Restart required.')
    expect(wrapper.text()).toContain('1 setting')
    expect(wrapper.text()).toContain('1970-01-01 00:00:00 UTC')
    expect(wrapper.text()).toContain('/listeners/a')
  })

  it('renders nothing when no restart is required', () => {
    const wrapper = mount(RestartBanner, { props: { restart: managementStatus().restartRequired } })
    expect(wrapper.text()).toBe('')
  })
})

describe('LimitForm', () => {
  const props = {
    label: 'Weight', subject: 'server a', applyLabel: 'Apply weight', resetLabel: 'Reset weight',
    placeholder: '1', min: 1, max: 100, disabled: false,
  }

  it('emits the number on submit and null-free reset on request', async () => {
    const wrapper = mount(LimitForm, { props })
    await wrapper.get('input').setValue('40')
    await wrapper.get('form').trigger('submit')
    await wrapper.findAll('button')[1]!.trigger('click')

    expect(wrapper.emitted('apply')).toEqual([[40]])
    expect(wrapper.emitted('reset')).toHaveLength(1)
  })

  it.each(['0', '101', '2.5'])('refuses %s', async (value) => {
    const wrapper = mount(LimitForm, { props })
    await wrapper.get('input').setValue(value)
    await flushPromises()
    await wrapper.get('form').trigger('submit')

    expect(wrapper.emitted('apply')).toBeUndefined()
    expect(wrapper.get('input').attributes('aria-invalid')).toBe('true')
  })

  it('has no upper bound without a maximum', async () => {
    const wrapper = mount(LimitForm, { props: { ...props, max: null } })
    await wrapper.get('input').setValue('100000')
    await wrapper.get('form').trigger('submit')
    expect(wrapper.emitted('apply')).toEqual([[100000]])
  })
})

describe('ServerCard', () => {
  it('emits typed commands for the entry', async () => {
    const server = entry('primary')
    const wrapper = mount(ServerCard, { props: { entry: server, disabled: false } })

    await wrapper.get('[data-state="drain"]').trigger('click')
    await wrapper.get('[data-health="up"]').trigger('click')
    await wrapper.get('[data-action="checks"]').trigger('click')
    await wrapper.get('[data-action="evacuate"]').trigger('click')

    expect(wrapper.emitted('command')).toEqual([
      [{ kind: 'state', entry: server, state: 'drain' }],
      [{ kind: 'health', entry: server, health: 'up' }],
      [{ kind: 'checks', entry: server }],
      [{ kind: 'evacuate', entry: server }],
    ])
  })

  it('offers DNS refresh only for a host name and checks only for a checked server', () => {
    const socket = mount(ServerCard, { props: { entry: entry('primary'), disabled: false } })
    const dns = mount(ServerCard, { props: { entry: entry('static-1'), disabled: false } })

    expect(socket.find('[data-action="dns"]').exists()).toBe(false)
    expect(socket.find('[data-action="checks"]').exists()).toBe(true)
    expect(dns.find('[data-action="dns"]').exists()).toBe(true)
    expect(dns.find('[data-action="checks"]').exists()).toBe(false)
  })

  it('shows a disabled-checks server with the way back', () => {
    const server = entry('static-1')
    server.server.checksEnabled = false
    const wrapper = mount(ServerCard, { props: { entry: server, disabled: false } })
    expect(wrapper.get('[data-action="checks"]').text()).toBe('Enable checks')
  })

  it('names administrative state and health with words', () => {
    const wrapper = mount(ServerCard, { props: { entry: entry('secondary'), disabled: false } })
    expect(wrapper.text()).toContain('Draining')
    expect(wrapper.text()).toContain('Unhealthy')
    expect(wrapper.text()).toContain('Forced down')
  })
})

describe('PoolTable', () => {
  it('shows a pool whose servers differ as mixed and emits pool commands', async () => {
    const { pools } = inventory()
    const wrapper = mount(PoolTable, { props: { pools: pools.pools, disabled: false } })

    expect(wrapper.get('[data-pool="web-backends"]').text()).toContain('Mixed: 1 of 2 not ready')
    expect(wrapper.findAll('[data-pool="web-backends"] [aria-pressed="true"]')).toHaveLength(0)
    expect(wrapper.get('[data-pool="static"] [data-state="ready"]').attributes('aria-pressed')).toBe('true')
    await wrapper.get('[data-pool="static"] [data-state="drain"]').trigger('click')
    await wrapper.get('[data-pool="static"] [aria-label="Evacuate pool static"]').trigger('click')
    expect(wrapper.emitted('command')).toEqual([
      [{ kind: 'pool-state', pool: pools.pools[1], state: 'drain' }],
      [{ kind: 'pool-evacuate', pool: pools.pools[1] }],
    ])
  })

  it('has an honest empty state', () => {
    const wrapper = mount(PoolTable, { props: { pools: [], disabled: false } })
    expect(wrapper.text()).toContain('No upstream pool inventory is available.')
  })
})

describe('ForceToggle', () => {
  it('emits the new force value', async () => {
    const wrapper = mount(ForceToggle, { props: { force: false } })
    await wrapper.get('input').setValue(true)
    expect(wrapper.emitted('update:force')).toEqual([[true]])
  })
})
