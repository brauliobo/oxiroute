import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import ThemeToggle from './ThemeToggle.vue'

describe('ThemeToggle', () => {
  it('chooses between automatic, light and dark', async () => {
    const wrapper = mount(ThemeToggle, { props: { modelValue: 'light' } })
    expect(wrapper.get('label').text()).toBe('Theme')
    expect(wrapper.findAll('option').map((option) => option.text())).toEqual(['Automatic', 'Light', 'Dark'])
    expect((wrapper.get('select').element as HTMLSelectElement).value).toBe('light')
    await wrapper.get('select').setValue('dark')
    expect(wrapper.emitted('update:modelValue')).toEqual([['dark']])
  })
})
