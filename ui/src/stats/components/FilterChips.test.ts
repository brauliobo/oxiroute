import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import FilterChips from './FilterChips.vue'

const props = { states: ['down'], problemsOnly: false }
const chip = (wrapper: ReturnType<typeof mount>, text: string) =>
  wrapper.findAll('button').find((button) => button.text() === text)!

describe('FilterChips', () => {
  it('names every server state in text and marks the active ones as pressed', () => {
    const wrapper = mount(FilterChips, { props })
    expect(wrapper.get('[role=group]').attributes('aria-labelledby')).toBe(wrapper.get('.ox-chips-label').attributes('id'))
    expect(wrapper.get('.ox-chips-label').text()).toBe('Server state')
    expect(wrapper.findAll('button').map((button) => button.text()))
      .toEqual(['UP', 'DOWN', 'DRAIN', 'MAINT', 'EJECTED', 'UNCHECKED', 'UNKNOWN', 'Problems only'])
    expect(chip(wrapper, 'DOWN').attributes('aria-pressed')).toBe('true')
    expect(chip(wrapper, 'UP').attributes('aria-pressed')).toBe('false')
  })

  it('emits the new state list when a chip is toggled', async () => {
    const wrapper = mount(FilterChips, { props })
    await chip(wrapper, 'UP').trigger('click')
    await chip(wrapper, 'DOWN').trigger('click')
    expect(wrapper.emitted('update:states')).toEqual([[['up', 'down']], [[]]])
  })

  it('toggles problems only', async () => {
    const wrapper = mount(FilterChips, { props: { ...props, problemsOnly: true } })
    expect(chip(wrapper, 'Problems only').attributes('aria-pressed')).toBe('true')
    await chip(wrapper, 'Problems only').trigger('click')
    expect(wrapper.emitted('update:problemsOnly')).toEqual([[false]])
  })
})
