import { describe, expect, it } from 'vitest'
import { applyTheme, nextTheme } from './theme'

describe('theme', () => {
  it('sets data-theme for an explicit theme and removes it for auto', () => {
    const root = document.createElement('html')
    applyTheme('dark', root)
    expect(root.dataset.theme).toBe('dark')
    applyTheme('light', root)
    expect(root.dataset.theme).toBe('light')
    applyTheme('auto', root)
    expect(root.hasAttribute('data-theme')).toBe(false)
  })

  it('cycles auto, light, dark', () => {
    expect(['auto', 'light', 'dark'].map((theme) => nextTheme(theme as 'auto'))).toEqual(['light', 'dark', 'auto'])
  })
})
