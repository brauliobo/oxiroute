import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Banner from './Banner.vue'
import Notice from './Notice.vue'

describe('Banner', () => {
  it('carries its tone as data and its text and actions as content', () => {
    const action = () => {
      const button = document.createElement('button')
      button.textContent = 'Retry now'
      return button
    }
    const wrapper = mount(Banner, { props: { tone: 'bad' }, slots: { default: 'Could not refresh.', actions: action } })
    expect(wrapper.attributes('data-tone')).toBe('bad')
    expect(wrapper.get('span').text()).toBe('Could not refresh.')
    expect(wrapper.get('button').text()).toBe('Retry now')
  })

  it('shows no action when none is given', () => {
    expect(mount(Banner, { props: { tone: 'warn' }, slots: { default: 'Stale.' } }).find('button').exists()).toBe(false)
  })
})

describe('Notice', () => {
  it('is a live region that exists before its message so that the message is announced', async () => {
    const status = mount(Notice, { props: { kind: 'status' } })
    expect(status.attributes('role')).toBe('status')
    expect(status.attributes('aria-atomic')).toBe('true')
    expect(status.text()).toBe('')
    expect(mount(Notice, { props: { kind: 'alert' }, slots: { default: 'Failed' } }).attributes('role')).toBe('alert')
  })
})
