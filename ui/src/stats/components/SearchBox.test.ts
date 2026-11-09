import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import SearchBox from './SearchBox.vue'

describe('SearchBox', () => {
  it('is a labelled search field that shows the current text', () => {
    const wrapper = mount(SearchBox, { props: { modelValue: 'origin' } })
    const input = wrapper.get('input[type=search]')
    expect(input.element.id).toBe('ox-search')
    expect(wrapper.get('label').attributes('for')).toBe('ox-search')
    expect((input.element as HTMLInputElement).value).toBe('origin')
  })

  it('emits what is typed', async () => {
    const wrapper = mount(SearchBox, { props: { modelValue: '' } })
    await wrapper.get('input').setValue('api')
    expect(wrapper.emitted('update:modelValue')).toEqual([['api']])
  })

  it('clears with Escape and leaves an empty field alone', async () => {
    const wrapper = mount(SearchBox, { props: { modelValue: 'api' } })
    await wrapper.get('input').trigger('keydown', { key: 'Escape' })
    expect(wrapper.emitted('update:modelValue')).toEqual([['']])
    await wrapper.setProps({ modelValue: '' })
    await wrapper.get('input').trigger('keydown', { key: 'Escape' })
    expect(wrapper.emitted('update:modelValue')).toHaveLength(1)
  })
})
