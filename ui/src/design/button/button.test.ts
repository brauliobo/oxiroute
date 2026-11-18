import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Button from './Button.vue'
import Chip from './Chip.vue'
import Toggle from './Toggle.vue'

describe('Button', () => {
  it('is a plain button that never submits a form by default, with its slot as the label', () => {
    const wrapper = mount(Button, { slots: { default: 'Refresh now' } })
    expect(wrapper.element.tagName).toBe('BUTTON')
    expect(wrapper.attributes('type')).toBe('button')
    expect(wrapper.text()).toBe('Refresh now')
    expect(wrapper.classes()).toEqual(['ox-btn'])
  })

  it('maps the variant and size to classes', () => {
    const wrapper = mount(Button, { props: { variant: 'primary', small: true }, slots: { default: 'Confirm' } })
    expect(wrapper.classes()).toEqual(['ox-btn', 'ox-primary', 'ox-small'])
  })

  it('reports a toggle state only when it has one', async () => {
    const wrapper = mount(Button, { slots: { default: 'Descending' } })
    expect(wrapper.attributes('aria-pressed')).toBeUndefined()
    await wrapper.setProps({ pressed: false })
    expect(wrapper.attributes('aria-pressed')).toBe('false')
    await wrapper.setProps({ pressed: true })
    expect(wrapper.attributes('aria-pressed')).toBe('true')
  })

  it('passes disabled, data attributes and click listeners to the button', async () => {
    let clicks = 0
    const wrapper = mount(Button, {
      props: { disabled: true, 'data-close': '', onClick: () => { clicks += 1 } },
      slots: { default: 'Cancel' },
    })
    expect(wrapper.attributes('disabled')).toBeDefined()
    expect(wrapper.attributes('data-close')).toBeDefined()
    await wrapper.setProps({ disabled: false })
    await wrapper.trigger('click')
    expect(clicks).toBe(1)
  })
})

describe('Chip', () => {
  it('is a pill button that always states whether it is pressed', async () => {
    const wrapper = mount(Chip, { props: { pressed: false }, slots: { default: 'Down' } })
    expect(wrapper.classes()).toEqual(['ox-chip'])
    expect(wrapper.attributes('aria-pressed')).toBe('false')
    await wrapper.setProps({ pressed: true })
    expect(wrapper.attributes('aria-pressed')).toBe('true')
    expect(wrapper.text()).toBe('Down')
  })
})

describe('Toggle', () => {
  it('discloses content: a decorative chevron, the label and the expanded state', async () => {
    const wrapper = mount(Toggle, { props: { expanded: false, 'aria-controls': 'details-1' }, slots: { default: 'web' } })
    expect(wrapper.attributes('aria-expanded')).toBe('false')
    expect(wrapper.attributes('aria-controls')).toBe('details-1')
    expect(wrapper.get('.ox-chevron').attributes('aria-hidden')).toBe('true')
    expect(wrapper.get('.ox-label').text()).toBe('web')
    await wrapper.setProps({ expanded: true })
    expect(wrapper.attributes('aria-expanded')).toBe('true')
  })
})
