import { describe, expect, it } from 'vitest'
import ListenerTable from './ListenerTable.vue'
import { documentAt, loadedStore, mountView } from './testing'

describe('ListenerTable', () => {
  it('lists listeners with state, connections and traffic, and opens their details with the routes they serve', async () => {
    const harness = await loadedStore([documentAt(0)])
    const wrapper = mountView(ListenerTable, harness)
    expect(wrapper.get('caption').text()).toContain('Listeners')
    const row = '[data-row="listener:public-http"]'
    expect(wrapper.get(`${row} [data-label=State]`).text()).toBe('LISTENING')
    expect(wrapper.get(`${row} [data-label=Connections]`).text()).toContain('7 / 1k')
    expect(wrapper.get(`${row} [data-label=Connections]`).text()).toContain('5k accepted')
    expect(wrapper.get(`${row} [data-label=Req\\/s]`).text()).toBe('—')
    expect(wrapper.get(`${row} [data-label=In]`).text()).toContain('7.6 MiB')
    expect(wrapper.get(`${row} [data-label=Latency]`).text()).toContain('p99')
    await wrapper.get(`${row} .ox-toggle`).trigger('click')
    const details = wrapper.get('.ox-details').text()
    for (const text of ['socket:192.0.2.10:8080', 'Outcomes', 'upstream_error30', 'Routes', 'site#0', 'example.com/', 'web']) {
      expect(details).toContain(text)
    }
    expect(wrapper.get('.ox-details .ox-routes a').attributes('href')).toMatch(/^#pool_/)
  })

  it('sorts by state, worst first', async () => {
    const harness = await loadedStore([documentAt(0, (wire) => {
      wire.listeners.push({ ...wire.listeners[0]!, name: 'broken', state: 'failed' })
    })])
    const wrapper = mountView(ListenerTable, harness)
    const labels = () => wrapper.findAll('.ox-row .ox-label').map((item) => item.text())
    expect(labels()).toEqual(['public-http', 'broken'])
    await wrapper.findAll('thead th').find((item) => item.text().startsWith('State'))!.get('button').trigger('click')
    expect(labels()).toEqual(['broken', 'public-http'])
  })

  it('shows a listener that is not listening as such, with a word', async () => {
    const wrapper = mountView(ListenerTable, await loadedStore([documentAt(0, (wire) => { wire.listeners[0]!.state = 'failed' })]))
    expect(wrapper.get('[data-label=State]').text()).toBe('FAILED')
  })

  it('says so when nothing is configured', async () => {
    const wrapper = mountView(ListenerTable, await loadedStore([documentAt(0, (wire) => { wire.listeners = [] })]))
    expect(wrapper.text()).toContain('No listeners are configured.')
  })
})

