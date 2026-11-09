import { describe, expect, it } from 'vitest'
import { SHORTCUTS, shortcutAction } from './shortcuts'

const press = (key: string, init: KeyboardEventInit = {}, target: Element = document.body) => {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, ...init })
  target.dispatchEvent(event)
  return event
}
const action = (key: string, init: KeyboardEventInit = {}, target: Element = document.body) =>
  shortcutAction(press(key, init, target))
const inside = (html: string) => {
  document.body.innerHTML = html
  return document.body.querySelector('[data-target]')!
}

describe('shortcutAction', () => {
  it('maps the documented keys', () => {
    expect(action('/')).toBe('search')
    expect(action('?', { shiftKey: true })).toBe('help')
    expect(action('p')).toBe('pause')
    expect(action('r')).toBe('refresh')
    expect(action('t')).toBe('theme')
    expect(action('f')).toBe('problems')
  })

  it('accepts capital letters (caps lock) and ignores other keys', () => {
    expect(action('P', { shiftKey: true })).toBe('pause')
    expect(action('x')).toBeUndefined()
    expect(action('Enter')).toBeUndefined()
  })

  it('leaves browser and system chords alone', () => {
    for (const modifier of ['ctrlKey', 'metaKey', 'altKey']) expect(action('r', { [modifier]: true })).toBeUndefined()
  })

  it('does not hijack typing in a field or a menu', () => {
    for (const html of ['<input data-target>', '<textarea data-target></textarea>', '<select data-target></select>',
      '<div data-target contenteditable="true"></div>']) {
      expect(action('p', {}, inside(html)), html).toBeUndefined()
    }
    expect(action('p', {}, inside('<button data-target>x</button>'))).toBe('pause')
  })

  it('does not act behind a dialog and ignores key repeat', () => {
    expect(action('p', {}, inside('<dialog open><button data-target>x</button></dialog>'))).toBeUndefined()
    expect(action('p', { repeat: true })).toBeUndefined()
  })

  it('documents every action with its key', () => {
    const keys = SHORTCUTS.map((shortcut) => shortcut.keys)
    expect(keys).toEqual(expect.arrayContaining(['/', '?', 'p', 'r', 't', 'f', 'Esc']))
    expect(new Set(keys).size).toBe(keys.length)
    expect(SHORTCUTS.every((shortcut) => shortcut.description.length > 10)).toBe(true)
  })
})
