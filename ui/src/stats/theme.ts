import { THEMES } from './urlState'
import type { Theme } from './urlState'

/** An explicit theme is `data-theme` on <html> (stats.css); `auto` removes it and follows prefers-color-scheme. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  if (theme === 'auto') root.removeAttribute('data-theme')
  else root.dataset.theme = theme
}

export const nextTheme = (theme: Theme): Theme => THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]!
