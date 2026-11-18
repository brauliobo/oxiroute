import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import NumberInput from './NumberInput.vue'

describe('NumberInput', () => {
  it('shows the number, reports typed numbers and passes native attributes through', async () => {
    const wrapper = mount(NumberInput, { props: { modelValue: 8, min: 1, max: 10, step: 1 } })
    const input = wrapper.get('input')
    expect(input.attributes('type')).toBe('number')
    expect(input.attributes('min')).toBe('1')
    expect((input.element as HTMLInputElement).value).toBe('8')
    await input.setValue('9')
    expect(wrapper.emitted('update:modelValue')).toEqual([[9]])
    await wrapper.setProps({ modelValue: 3 })
    expect((input.element as HTMLInputElement).value).toBe('3')
  })

  it('shows an empty field for null and does not rewrite what the user is typing', async () => {
    const wrapper = mount(NumberInput, { props: { modelValue: null } })
    const element = wrapper.get('input').element as HTMLInputElement
    expect(element.value).toBe('')
    element.value = '1.50'
    await wrapper.get('input').trigger('input')
    await wrapper.setProps({ modelValue: 1.5 })
    expect(element.value).toBe('1.50')
  })

  it('reports the raw text when it is not a number', async () => {
    const wrapper = mount(NumberInput, { props: { modelValue: 1 } })
    const element = wrapper.get('input').element as HTMLInputElement
    element.value = ''
    await wrapper.get('input').trigger('input')
    expect(wrapper.emitted('update:modelValue')).toEqual([['']])
  })
})

