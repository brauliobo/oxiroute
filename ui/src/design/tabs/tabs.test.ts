import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Tabs from './Tabs.vue'

const TABS = [{ id: 'one', label: 'One' }, { id: 'two', label: 'Two' }, { id: 'three', label: 'Three' }]
const open = (modelValue = 'one') => mount(Tabs, { props: { tabs: TABS, modelValue, label: 'Sections' }, attachTo: document.body, slots: { default: 'panel' } })
const press = (wrapper: ReturnType<typeof open>, key: string) => wrapper.get('[role=tab][aria-selected=true]').trigger('keydown', { key })

describe('Tabs', () => {
  it('is a labelled tablist whose selected tab owns the panel', () => {
    const wrapper = open('two')
    expect(wrapper.get('[role=tablist]').attributes('aria-label')).toBe('Sections')
    const tabs = wrapper.findAll('[role=tab]')
    expect(tabs.map((tab) => tab.attributes('aria-selected'))).toEqual(['false', 'true', 'false'])
    expect(tabs.map((tab) => tab.attributes('tabindex'))).toEqual(['-1', '0', '-1'])
    const panel = wrapper.get('[role=tabpanel]')
    expect(panel.attributes('aria-labelledby')).toBe(tabs[1]!.attributes('id'))
    expect(tabs.every((tab) => tab.attributes('aria-controls') === panel.attributes('id'))).toBe(true)
    expect(panel.text()).toBe('panel')
  })

  it('selects a tab on click', async () => {
    const wrapper = open()
    await wrapper.findAll('[role=tab]')[2]!.trigger('click')
    expect(wrapper.emitted('update:modelValue')).toEqual([['three']])
  })

  it('moves with the arrow keys, wrapping at both ends, and with Home and End, focusing the new tab', async () => {
    const wrapper = open('three')
    await press(wrapper, 'ArrowRight')
    await wrapper.setProps({ modelValue: 'one' })
    await press(wrapper, 'ArrowLeft')
    await press(wrapper, 'End')
    await press(wrapper, 'Home')
    expect(wrapper.emitted('update:modelValue')).toEqual([['one'], ['three'], ['three'], ['one']])
    expect(document.activeElement?.getAttribute('role')).toBe('tab')
  })

  it('ignores other keys', async () => {
    const wrapper = open()
    await press(wrapper, 'a')
    expect(wrapper.emitted('update:modelValue')).toBeUndefined()
  })
})
