import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import postcss from 'postcss'
import valueParser from 'postcss-value-parser'
import { describe, expect, it } from 'vitest'

type Rgb = readonly [number, number, number]
type Theme = 'light' | 'dark'
type Pair = Readonly<Record<Theme, string>>

const dir = import.meta.dirname
const parse = (file: string) => postcss.parse(readFileSync(join(dir, file), 'utf8'))

function declarations(file: string): Map<string, string> {
  const found = new Map<string, string>()
  parse(file).walkDecls(/^--ox-/, (decl) => void found.set(decl.prop, decl.value))
  return found
}

function colorPairs(): Map<string, Pair> {
  const pairs = new Map<string, Pair>()
  for (const [name, value] of declarations('color.css')) {
    const [node] = valueParser(value).nodes
    if (node?.type !== 'function' || node.value !== 'light-dark') continue
    const [light, dark] = node.nodes.filter((part) => part.type === 'word').map((part) => part.value)
    pairs.set(name.slice('--ox-'.length), { light: light!, dark: dark! })
  }
  return pairs
}

const pairs = colorPairs()
const themes: readonly Theme[] = ['light', 'dark']

function rgb(hex: string): Rgb {
  expect(hex).toMatch(/^#[0-9a-f]{6}$/)
  return [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16)) as unknown as Rgb
}

const linear = (channel: number) => {
  const value = channel / 255
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
}
const luminance = ([r, g, b]: Rgb) => 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
const contrast = (a: Rgb, b: Rgb) => {
  const [high, low] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (high! + 0.05) / (low! + 0.05)
}
const tint = (foreground: Rgb, background: Rgb, share: number): Rgb =>
  foreground.map((channel, at) => Math.round(channel * share + background[at]! * (1 - share))) as unknown as Rgb

const color = (name: string, theme: Theme) => rgb(pairs.get(name)![theme])
const ratio = (foreground: string, background: string, theme: Theme) => contrast(color(foreground, theme), color(background, theme))

const TEXT = ['fg', 'muted', 'ok', 'warn', 'bad', 'info', 'brand']
const TONES = ['muted', 'ok', 'warn', 'bad', 'info', 'brand']
const SURFACES = ['bg', 'surface']
const BADGE_TINT = 0.1

describe('colour tokens', () => {
  it('define every colour as one light-dark() pair of six-digit hex values', () => {
    for (const name of ['bg', 'surface', 'fg', 'muted', 'border', 'control', 'ok', 'warn', 'bad', 'info', 'brand', 'on-brand']) {
      expect(pairs.has(name), name).toBe(true)
    }
    for (const name of pairs.keys()) for (const theme of themes) color(name, theme)
  })

  it('differ between the themes', () => {
    for (const [name, pair] of pairs) expect(pair.light, name).not.toBe(pair.dark)
  })

  it.each(themes)('text and status colours reach 4.5:1 on the page and card backgrounds (%s)', (theme) => {
    for (const foreground of TEXT) {
      for (const background of SURFACES) {
        expect(ratio(foreground, background, theme), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5)
      }
    }
  })

  it.each(themes)('badge text reaches 4.5:1 on its own tinted background over the page and card (%s)', (theme) => {
    for (const foreground of TONES) {
      for (const background of SURFACES) {
        const tinted = tint(color(foreground, theme), color(background, theme), BADGE_TINT)
        expect(contrast(color(foreground, theme), tinted), `${foreground} on ${background}`).toBeGreaterThanOrEqual(4.5)
      }
    }
  })

  it.each(themes)('text on the brand fill reaches 4.5:1 (%s)', (theme) => {
    expect(ratio('on-brand', 'brand', theme)).toBeGreaterThanOrEqual(4.5)
  })

  it.each(themes)('control borders, focus ring and brand reach 3:1 against the page and card (%s)', (theme) => {
    for (const foreground of ['control', 'brand', 'info']) {
      for (const background of SURFACES) {
        expect(ratio(foreground, background, theme), `${foreground} on ${background}`).toBeGreaterThanOrEqual(3)
      }
    }
  })
})

describe('token files', () => {
  const files = readdirSync(dir).filter((file) => file.endsWith('.css'))

  it('hold the six families', () => {
    expect(files.sort()).toEqual(['color.css', 'density.css', 'motion.css', 'shape.css', 'space.css', 'type.css'])
  })

  it.each(files)('%s declares only --ox-* custom properties inside the ox.tokens layer', (file) => {
    const root = parse(file)
    const layers = root.nodes.filter((node) => node.type === 'atrule')
    expect(layers.map((layer) => layer.params)).toEqual(['ox.tokens'])
    root.walkDecls((decl) => expect(decl.prop, decl.toString()).toMatch(/^(--ox-|color-scheme$)/))
  })

  it('give every density the same set of properties', () => {
    const density = parse('density.css')
    const sets: string[][] = []
    density.walkRules(/data-density/, (rule) => {
      const names: string[] = []
      rule.walkDecls((decl) => void names.push(decl.prop))
      if (rule.parent?.type === 'atrule' && (rule.parent as postcss.AtRule).name === 'layer') sets.push(names.sort())
    })
    expect(sets).toHaveLength(2)
    expect(sets[0]).toEqual(sets[1])
  })
})
