import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Footer from './Footer.vue'

describe('Footer', () => {
  it('is the page footer with the version, the generation and the time of the sample', () => {
    const wrapper = mount(Footer, { props: { version: '6.3.0', generation: 7, sampledAtUnixMs: 1_800_000_000_000 } })
    expect(wrapper.get('footer').attributes('role')).toBeUndefined()
    const text = wrapper.text()
    expect(text).toContain('OxiRoute 6.3.0')
    expect(text).toContain('generation 7')
    expect(text).toContain('2027-01-15')
  })

  it('shows nothing invented before the first snapshot and still offers the shortcuts', async () => {
    const wrapper = mount(Footer, { props: {} })
    expect(wrapper.text()).not.toContain('generation')
    await wrapper.get('button').trigger('click')
    expect(wrapper.get('button').text()).toContain('Keyboard shortcuts')
    expect(wrapper.emitted('help')).toHaveLength(1)
  })
})
