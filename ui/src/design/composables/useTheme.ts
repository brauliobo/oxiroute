import { ref, watch, watchEffect } from 'vue'
import type { Ref } from 'vue'

export const THEMES = ['auto', 'light', 'dark'] as const
export type Theme = (typeof THEMES)[number]

export const THEME_LABELS: Readonly<Record<Theme, string>> = { auto: 'Automatic', light: 'Light', dark: 'Dark' }

const STORAGE_KEY = 'oxiroute.theme'

export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  if (theme === 'auto') root.removeAttribute('data-theme')
  else root.dataset.theme = theme
}

export const nextTheme = (theme: Theme): Theme => THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]!

export function useTheme(theme: Ref<Theme>): void {
  watchEffect(() => applyTheme(theme.value))
}

export function useStoredTheme(storage: Pick<Storage, 'getItem' | 'setItem'> = localStorage): Ref<Theme> {
  const stored = storage.getItem(STORAGE_KEY)
  const theme = ref<Theme>(THEMES.find((item) => item === stored) ?? 'auto')
  watch(theme, (next) => storage.setItem(STORAGE_KEY, next))
  useTheme(theme)
  return theme
}
