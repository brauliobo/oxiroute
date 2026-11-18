import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import EmptyState from './EmptyState.vue'
import Loading from './Loading.vue'

describe('EmptyState', () => {
  it('says why nothing is shown, with an optional title', () => {
    const wrapper = mount(EmptyState, { props: { title: 'No pools' }, slots: { default: 'No pools are configured.' } })
    expect(wrapper.get('strong').text()).toBe('No pools')
    expect(wrapper.text()).toContain('No pools are configured.')
  })

  it('needs no title', () => {
    expect(mount(EmptyState, { slots: { default: 'Nothing here.' } }).find('strong').exists()).toBe(false)
  })
})

describe('Loading', () => {
  it('announces itself politely and hides the spinner from assistive technology', () => {
    const wrapper = mount(Loading)
    expect(wrapper.attributes('role')).toBe('status')
    expect(wrapper.get('.ox-spinner').attributes('aria-hidden')).toBe('true')
    expect(wrapper.text()).toBe('Loading')
  })

  it('names what is loading', () => {
    expect(mount(Loading, { props: { label: 'Loading certificates' } }).text()).toBe('Loading certificates')
  })
})
