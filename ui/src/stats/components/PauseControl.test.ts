import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import PauseControl from './PauseControl.vue'

describe('PauseControl', () => {
  it('pauses a running page and resumes a paused one', async () => {
    const wrapper = mount(PauseControl, { props: { paused: false } })
    expect(wrapper.findAll('button').map((button) => button.text())).toEqual(['Pause', 'Refresh now'])
    await wrapper.findAll('button')[0]!.trigger('click')
    await wrapper.setProps({ paused: true })
    expect(wrapper.findAll('button')[0]!.text()).toBe('Resume')
    expect(wrapper.emitted('toggle')).toHaveLength(1)
  })

  it('refreshes once on request', async () => {
    const wrapper = mount(PauseControl, { props: { paused: true } })
    await wrapper.findAll('button')[1]!.trigger('click')
    expect(wrapper.emitted('refresh')).toHaveLength(1)
  })
})
