import { describe, expect, it } from 'vitest'
import { flushPromises } from '../../test/mount'
import StatsDashboard from './StatsDashboard.vue'
import { documentAt, loadedStore, mountView, waitingStore } from './testing'

describe('StatsDashboard', () => {
  it('shows nothing before the first snapshot', async () => {
    const wrapper = mountView(StatsDashboard, waitingStore())
    expect(wrapper.find('.ox-dashboard').exists()).toBe(false)
  })

  it('shows the views in reading order, each under a heading', async () => {
    const wrapper = mountView(StatsDashboard, await loadedStore([documentAt(0)]))
    await flushPromises()
    expect(wrapper.findAll('h2').map((item) => item.text())).toEqual(
      ['Listeners', 'Routes', 'Pools and servers', 'Generation 4', 'Process', 'Certificates', 'Warnings'])
    expect(wrapper.find('.ox-overview').exists()).toBe(true)
  })

  it('has no inline style attribute except the custom properties the bars set', async () => {
    const wrapper = mountView(StatsDashboard, await loadedStore([documentAt(0)]))
    const styled = Array.from(wrapper.element.querySelectorAll('[style]'))
    expect(styled.length).toBeGreaterThan(0)
    expect(styled.every((element) => /^(--[\w-]+:\s*[\d.]+;?\s*)+$/.test(element.getAttribute('style')!))).toBe(true)
  })
})
