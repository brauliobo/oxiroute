import { nextTick, ref } from 'vue'
import { describe, expect, it } from 'vitest'
import { THEMES, applyTheme, nextTheme, useStoredTheme, useTheme } from './useTheme'
import type { Theme } from './useTheme'

const memory = (initial?: string) => {
  const values = new Map<string, string>(initial ? [['oxiroute.theme', initial]] : [])
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => void values.set(key, value), values }
}

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
    expect(THEMES.map(nextTheme)).toEqual(['light', 'dark', 'auto'])
  })

  it('keeps <html data-theme> in step with a reactive theme', async () => {
    const theme = ref<Theme>('dark')
    useTheme(theme)
    expect(document.documentElement.dataset.theme).toBe('dark')
    theme.value = 'auto'
    await nextTick()
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false)
  })

  it('restores a stored choice, ignores an unknown value and stores changes', async () => {
    const storage = memory('light')
    const theme = useStoredTheme(storage)
    expect(theme.value).toBe('light')
    theme.value = 'dark'
    await nextTick()
    expect(storage.values.get('oxiroute.theme')).toBe('dark')
    expect(useStoredTheme(memory('neon')).value).toBe('auto')
  })
})
