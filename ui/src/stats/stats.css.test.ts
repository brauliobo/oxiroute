import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

// The stylesheet is checked as text: tokens exist with the names the data views use, every pair that carries text or a
// control border has enough contrast in both themes, and the accessibility and print rules are present.
const css = readFileSync(join(import.meta.dirname, 'stats.css'), 'utf8')
const tokens = new Map<string, [string, string]>()
for (const [, name, light, dark] of css.matchAll(/--ox-([a-z-]+):\s*light-dark\((#[0-9a-f]{6}),\s*(#[0-9a-f]{6})\)/g)) {
  tokens.set(name!, [light!, dark!])
}

const luminance = (hex: string) => {
  const [r, g, b] = [1, 3, 5].map((at) => {
    const channel = parseInt(hex.slice(at, at + 2), 16) / 255
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}
const contrast = (a: string, b: string) => {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (high! + 0.05) / (low! + 0.05)
}
const pair = (foreground: string, background: string, theme: 0 | 1) => contrast(tokens.get(foreground)![theme], tokens.get(background)![theme])

describe('stats.css tokens', () => {
  it('defines every token the data views use, with a light and a dark value', () => {
    for (const name of ['bg', 'fg', 'muted', 'border', 'ok', 'warn', 'bad', 'info', 'surface']) expect(tokens.has(name), name).toBe(true)
  })

  it.each([0, 1] as const)('text and status colours reach 4.5:1 on the page and card backgrounds (theme %i)', (theme) => {
    for (const foreground of ['fg', 'muted', 'ok', 'warn', 'bad', 'info']) {
      for (const background of ['bg', 'surface']) {
        expect(pair(foreground, background, theme), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5)
      }
    }
  })

  it.each([0, 1] as const)('control borders reach 3:1 against the surface (theme %i)', (theme) => {
    expect(pair('control', 'surface', theme)).toBeGreaterThanOrEqual(3)
  })

  it('the two themes differ', () => {
    for (const [name, [light, dark]] of tokens) expect(light, name).not.toBe(dark)
  })
})

describe('stats.css rules', () => {
  it('follows the system theme and lets data-theme override it', () => {
    expect(css).toMatch(/:root\s*{[^}]*color-scheme:\s*light dark/)
    expect(css).toMatch(/:root\[data-theme='light'\]\s*{\s*color-scheme:\s*light/)
    expect(css).toMatch(/:root\[data-theme='dark'\]\s*{\s*color-scheme:\s*dark/)
  })

  it('keeps a visible focus ring, honours forced colours, reduced motion and print', () => {
    expect(css).toContain(':focus-visible')
    expect(css).toContain('@media (forced-colors: active)')
    expect(css).toContain('@media (prefers-contrast: more)')
    expect(css).toContain('@media (prefers-reduced-motion: reduce)')
    expect(css).toContain('@media print')
    expect(css).toContain('@media (max-width: 720px)')
  })

  it('loads nothing from the network', () => {
    expect(css).not.toMatch(/url\(|@import|https?:/)
  })

  it('stays a reviewable size', () => {
    expect(css.split('\n').length).toBeLessThanOrEqual(400)
  })
})
