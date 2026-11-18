import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Card from './Card.vue'
import Panel from './Panel.vue'

describe('Panel', () => {
  it('is a region named by its own heading', () => {
    const wrapper = mount(Panel, { props: { title: 'Warnings', id: 'warnings' }, slots: { default: 'No warnings.' } })
    const heading = wrapper.get('h2')
    expect(wrapper.element.tagName).toBe('SECTION')
    expect(wrapper.attributes('id')).toBe('warnings')
    expect(wrapper.attributes('aria-labelledby')).toBe(heading.attributes('id'))
    expect(heading.text()).toBe('Warnings')
    expect(wrapper.text()).toContain('No warnings.')
  })

})

describe('Card', () => {
  it('is a surface that takes its role and name from the page', () => {
    const wrapper = mount(Card, { props: { role: 'search', 'aria-label': 'Filter and view' }, slots: { default: 'controls' } })
    expect(wrapper.classes()).toContain('ox-card')
    expect(wrapper.attributes('role')).toBe('search')
    expect(wrapper.attributes('aria-label')).toBe('Filter and view')
    expect(wrapper.text()).toBe('controls')
  })
})
