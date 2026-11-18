import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Checkbox from './Checkbox.vue'
import Field from './Field.vue'
import Input from './Input.vue'
import Select from './Select.vue'

const OPTIONS = [{ value: 'auto', label: 'Automatic' }, { value: 'dark', label: 'Dark' }]

describe('Field', () => {
  it('ties its label to the control by id', () => {
    const wrapper = mount(Field, { props: { label: 'Theme', labelFor: 'theme' }, slots: { default: () => document.createElement('select') } })
    expect(wrapper.get('label').attributes('for')).toBe('theme')
    expect(wrapper.get('label').text()).toBe('Theme')
    expect(wrapper.get('label').classes()).not.toContain('ox-sr-only')
  })

  it('can hide the label visually and keep it for assistive technology', () => {
    const wrapper = mount(Field, { props: { label: 'Search', labelFor: 'q', hideLabel: true } })
    expect(wrapper.get('label').classes()).toContain('ox-sr-only')
  })

  it('is a bare group of controls without a label', () => {
    expect(mount(Field).find('label').exists()).toBe(false)
  })
})

describe('Input', () => {
  it('shows the value, reports typing and passes native attributes through', async () => {
    const wrapper = mount(Input, { props: { modelValue: 'web', type: 'search', placeholder: 'Search', maxlength: 20 } })
    const input = wrapper.get('input')
    expect((input.element as HTMLInputElement).value).toBe('web')
    expect(input.attributes('type')).toBe('search')
    expect(input.attributes('placeholder')).toBe('Search')
    expect(input.attributes('maxlength')).toBe('20')
    await input.setValue('api')
    expect(wrapper.emitted('update:modelValue')).toEqual([['api']])
    await wrapper.setProps({ modelValue: 'cache' })
    expect((input.element as HTMLInputElement).value).toBe('cache')
  })

  it('is a text field unless told otherwise', () => {
    expect(mount(Input, { props: { modelValue: '' } }).attributes('type')).toBe('text')
  })
})

describe('Select', () => {
  it('lists the options with the current one selected and reports a choice', async () => {
    const wrapper = mount(Select, { props: { modelValue: 'dark', options: OPTIONS, id: 'theme' } })
    expect(wrapper.attributes('id')).toBe('theme')
    expect(wrapper.findAll('option').map((option) => option.text())).toEqual(['Automatic', 'Dark'])
    expect((wrapper.element as HTMLSelectElement).value).toBe('dark')
    await wrapper.setValue('auto')
    expect(wrapper.emitted('update:modelValue')).toEqual([['auto']])
  })

  it('follows a new value from outside', async () => {
    const wrapper = mount(Select, { props: { modelValue: 'dark', options: OPTIONS } })
    await wrapper.setProps({ modelValue: 'auto' })
    expect((wrapper.element as HTMLSelectElement).value).toBe('auto')
  })
})

describe('Checkbox', () => {
  it('is a labelled native checkbox that reports its state', async () => {
    const wrapper = mount(Checkbox, { props: { modelValue: false }, slots: { default: 'Problems only' } })
    expect(wrapper.get('label').text()).toBe('Problems only')
    const box = wrapper.get('input')
    expect(box.attributes('type')).toBe('checkbox')
    await box.setValue(true)
    expect(wrapper.emitted('update:modelValue')).toEqual([[true]])
  })

  it('can be disabled', () => {
    expect(mount(Checkbox, { props: { modelValue: true, disabled: true } }).get('input').attributes('disabled')).toBeDefined()
  })
})
