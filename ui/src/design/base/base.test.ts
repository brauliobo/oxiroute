import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import postcss from 'postcss'
import { describe, expect, it } from 'vitest'

const dir = import.meta.dirname
const files = readdirSync(dir).filter((file) => file.endsWith('.css'))
const parse = (file: string) => postcss.parse(readFileSync(join(dir, file), 'utf8'))

function atRules(file: string, name: string): string[] {
  const found: string[] = []
  parse(file).walkAtRules(name, (rule) => void found.push(rule.params))
  return found
}

function selectors(file: string): string[] {
  const found: string[] = []
  parse(file).walkRules((rule) => void found.push(rule.selector))
  return found
}

describe('design base styles', () => {
  it('hold one file per concern', () => {
    expect(files.sort()).toEqual([
      'contrast.css', 'document.css', 'focus.css', 'forced.css', 'motion.css', 'noscript.css', 'print.css', 'reset.css',
    ])
  })

  it.each(files)('%s lives entirely in one cascade layer', (file) => {
    const roots = parse(file).nodes.filter((node) => node.type !== 'comment')
    expect(roots).toHaveLength(1)
    expect(roots[0]).toMatchObject({ type: 'atrule', name: 'layer' })
  })

  it.each(files)('%s loads nothing from the network and hard-codes no colour', (file) => {
    const css = readFileSync(join(dir, file), 'utf8')
    expect(css).not.toMatch(/url\(|@import|https?:|#[0-9a-f]{3,8}\b|rgb\(|hsl\(/i)
  })

  it('keeps a visible focus ring on every focusable element', () => {
    expect(selectors('focus.css')).toEqual([':focus-visible'])
  })

  it('answers forced colours, more contrast, reduced motion and print', () => {
    expect(atRules('forced.css', 'media')).toEqual(['(forced-colors: active)'])
    expect(atRules('contrast.css', 'media')).toEqual(['(prefers-contrast: more)'])
    expect(atRules('motion.css', 'media')).toEqual(['(prefers-reduced-motion: reduce)'])
    expect(atRules('print.css', 'media')).toEqual(['print'])
  })

  it('styles the server-rendered table of the no-script fallback without any class', () => {
    const all = selectors('noscript.css')
    expect(all).toContain('noscript table')
    expect(all.every((selector) => selector.startsWith('noscript'))).toBe(true)
  })
})
