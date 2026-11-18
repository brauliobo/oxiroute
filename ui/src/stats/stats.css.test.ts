import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import postcss from 'postcss'
import { describe, expect, it } from 'vitest'

const dir = import.meta.dirname
const files = [
  join(dir, 'stats.css'),
  ...readdirSync(join(dir, 'components')).filter((file) => file.endsWith('.css')).map((file) => join(dir, 'components', file)),
]

describe('stats page styles', () => {
  it.each(files)('%s lives entirely in the page layer and loads nothing from the network', (file) => {
    const css = readFileSync(file, 'utf8')
    const roots = postcss.parse(css).nodes.filter((node) => node.type !== 'comment')
    expect(roots).toHaveLength(1)
    expect(roots[0]).toMatchObject({ type: 'atrule', name: 'layer', params: 'ox.page' })
    expect(css).not.toMatch(/url\(|@import|https?:/)
  })

  it('keeps a narrow-screen layout and hides the controls on paper', () => {
    const css = readFileSync(join(dir, 'stats.css'), 'utf8')
    expect(css).toContain('@media (max-width: 45rem)')
    expect(css).toContain('@media print')
  })
})
