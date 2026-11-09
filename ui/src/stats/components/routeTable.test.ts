import { describe, expect, it } from 'vitest'
import PoolTable from './PoolTable.vue'
import RouteTable from './RouteTable.vue'
import { documentAt, loadedStore, mountView } from './testing'

describe('RouteTable', () => {
  it('lists the routes with their selectors and a link to the pool they use', async () => {
    const wrapper = mountView(RouteTable, await loadedStore([documentAt(0, (wire) => {
      wire.routes.push({
        id: 'site#1', service: 'site', index: 1, listeners: ['public-http'], path: { kind: 'exact', value: '/old' }, methods: ['GET', 'HEAD'],
        action: 'redirect', status: 301,
      })
    })]))
    const first = '[data-row="route:site%231"]'
    expect(wrapper.get('caption').text()).toContain('Routes')
    expect(wrapper.findAll('tbody.ox-row')).toHaveLength(2)
    expect(wrapper.get('[data-row="route:site%230"] [data-label=Host]').text()).toContain('example.com')
    expect(wrapper.get('[data-row="route:site%230"] [data-label=Target] a').text()).toBe('web')
    expect(wrapper.get(`${first} [data-label=Target]`).text()).toBe('status 301')
    expect(wrapper.get(`${first} [data-label=Methods]`).text()).toBe('GET, HEAD')
    expect(wrapper.get(`${first} [data-label=Host]`).text()).toBe('any')
    expect(wrapper.get(`${first} [data-label=Retries]`).text()).toBe('—')
  })

  it('links to an anchor that exists in the pool table', async () => {
    const harness = await loadedStore([documentAt(0)])
    const routes = mountView(RouteTable, harness)
    const pools = mountView(PoolTable, harness, {})
    const href = routes.get('a').attributes('href')!
    expect(pools.find(href).exists()).toBe(true)
  })
})
