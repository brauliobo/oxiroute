import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import StatusBanner from './StatusBanner.vue'

describe('StatusBanner', () => {
  it('keeps an empty alert region in the page so changes are announced', () => {
    const wrapper = mount(StatusBanner, { props: { banner: undefined } })
    expect(wrapper.get('[role=alert]').text()).toBe('')
    expect(wrapper.find('button').exists()).toBe(false)
  })

  it('shows the text with its tone and a retry that emits', async () => {
    const banner = { tone: 'warn', text: 'Could not refresh.', action: 'retry' }
    const wrapper = mount(StatusBanner, { props: { banner } })
    expect(wrapper.get('[role=alert]').text()).toContain('Could not refresh.')
    expect(wrapper.get('.ox-banner').attributes('data-tone')).toBe('warn')
    await wrapper.get('button').trigger('click')
    expect(wrapper.get('button').text()).toBe('Retry now')
    expect(wrapper.emitted('action')).toEqual([['retry']])
  })

  it('offers a reload for an unsupported version', async () => {
    const wrapper = mount(StatusBanner, { props: { banner: { tone: 'bad', text: 'Old.', action: 'reload' } } })
    expect(wrapper.get('button').text()).toBe('Reload page')
    await wrapper.get('button').trigger('click')
    expect(wrapper.emitted('action')).toEqual([['reload']])
  })
})
