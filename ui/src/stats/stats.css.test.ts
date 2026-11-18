import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const css = readFileSync(join(import.meta.dirname, 'stats.css'), 'utf8')

describe('stats.css rules', () => {
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
})
