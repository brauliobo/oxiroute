import { describe, expect, it, vi } from 'vitest'
import { defineVaporComponent } from 'vue'
import { mount } from '../../test/mount'
import { shortcutFor, useShortcuts } from './useShortcuts'

const KEYMAP = { p: 'pause', r: 'refresh' } as const

const press = (key: string, init: KeyboardEventInit = {}, target: Element = document.body) => {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init })
  target.dispatchEvent(event)
  return event
}
const inside = (html: string) => {
  document.body.innerHTML = html
  return document.body.querySelector('[data-target]')!
}

describe('shortcutFor', () => {
  it('maps keys case-insensitively and ignores unmapped keys', () => {
    expect(shortcutFor(press('p'), KEYMAP)).toBe('pause')
    expect(shortcutFor(press('R', { shiftKey: true }), KEYMAP)).toBe('refresh')
    expect(shortcutFor(press('x'), KEYMAP)).toBeUndefined()
  })

  it('leaves browser chords and key repeat alone', () => {
    for (const modifier of ['ctrlKey', 'metaKey', 'altKey']) {
      expect(shortcutFor(press('r', { [modifier]: true }), KEYMAP)).toBeUndefined()
    }
    expect(shortcutFor(press('p', { repeat: true }), KEYMAP)).toBeUndefined()
  })

  it('does not hijack typing or act behind a dialog', () => {
    for (const html of ['<input data-target>', '<textarea data-target></textarea>', '<select data-target></select>',
      '<div data-target contenteditable="true"></div>', '<dialog open><button data-target>x</button></dialog>']) {
      expect(shortcutFor(press('p', {}, inside(html)), KEYMAP), html).toBeUndefined()
    }
    expect(shortcutFor(press('p', {}, inside('<button data-target>x</button>')), KEYMAP)).toBe('pause')
  })
})

describe('useShortcuts', () => {
  it('runs the handler of a pressed key while mounted and stops after unmount', () => {
    const pause = vi.fn()
    const Probe = defineVaporComponent(() => {
      useShortcuts(KEYMAP, { pause, refresh: () => undefined })
      return document.createElement('div')
    })
    const wrapper = mount(Probe)
    expect(press('p').defaultPrevented).toBe(true)
    expect(pause).toHaveBeenCalledTimes(1)
    wrapper.unmount()
    press('p')
    expect(pause).toHaveBeenCalledTimes(1)
  })
})
