import { describe, expect, it } from 'vitest'
import { mount } from '../../test/mount'
import Kbd from './Kbd.vue'

describe('Kbd', () => {
  it('marks up a key as keyboard input', () => {
    const wrapper = mount(Kbd, { slots: { default: 'Esc' } })
    expect(wrapper.element.tagName).toBe('KBD')
    expect(wrapper.text()).toBe('Esc')
  })
})
