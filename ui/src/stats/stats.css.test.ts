import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const css = readFileSync(join(import.meta.dirname, 'stats.css'), 'utf8')

describe('stats.css rules', () => {
  it('keeps a narrow-screen layout, forced colours and print rules for the page chrome', () => {
    expect(css).toContain('@media (forced-colors: active)')
    expect(css).toContain('@media print')
    expect(css).toContain('@media (max-width: 720px)')
  })

  it('loads nothing from the network', () => {
    expect(css).not.toMatch(/url\(|@import|https?:/)
  })
})
