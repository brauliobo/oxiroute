import { posix } from 'node:path'
import { parse } from 'vue/compiler-sfc'
import { describe, expect, it } from 'vitest'
import rawElementBaseline from './sfcPolicy.baseline.json'

// Every SFC is Vapor, so templates may only use what the Vapor production compiler supports. `$emit` and friends
// compile in development and in tests but break the production bundle at runtime, so they are checked on the source.
const sources = import.meta.glob<string>('./**/*.vue', { eager: true, query: '?raw', import: 'default' })
const files = Object.entries(sources).map(([path, source]) => ({ path, descriptor: parse(source).descriptor }))

const owned = import.meta.glob<string>('./{design,stats,gallery}/**/*.{ts,vue,css}', { eager: true, query: '?raw', import: 'default' })
const ownedFiles = Object.entries(owned).map(([path, source]) => ({ path: path.slice(2), source }))
const nonStandardFiles = Object.keys(import.meta.glob('./**/*.{tsx,jsx}'))

const templateRules: Array<[string, RegExp]> = [
  ['instance globals such as $emit, $props, $attrs and $slots (bind defineEmits/defineProps/useAttrs/useSlots)',
    /(?<![\w$.])\$(data|props|el|options|parent|root|slots|refs|attrs|watch|emit|forceUpdate|nextTick)\b/],
  ['inline styles (:style, style=) are blocked by a strict style-src; use classes or v-css-vars',
    /(?<![\w-])(?::|v-bind:)?style\s*=|:style\b/],
  ['v-html', /\bv-html\b/],
  ['v-memo (unsupported by Vapor)', /\bv-memo\b/],
  ['Suspense (unsupported by pure Vapor apps)', /\bSuspense\b/],
]
const scriptRules: Array<[string, RegExp]> = [
  ['getCurrentInstance (null in Vapor)', /\bgetCurrentInstance\b/],
  ['app.config.globalProperties (unsupported by Vapor)', /\bglobalProperties\b/],
  ['top-level await (needs Suspense)', /^await\b/m],
  ['render functions and h() (templates are Pug; build a primitive instead)', /\bh\(|\brender\s*:/],
  ['defineComponent (components are <script setup vapor>)', /\bdefineComponent\b/],
]

const RAW_ELEMENT = /(?:^[ \t]*|#\[)(button|input|select|textarea|table|dialog)(?=$|[\s.#(:\]])/gm
const rawElements = (template: string) => [...template.matchAll(RAW_ELEMENT)].length
const IMPORT = /(?:\bfrom|\bimport|@import)\s*\(?\s*['"]([^'"]+)['"]/g

const TEST_SUPPORT = /(\.test|\/testing)\.ts$/

const ALLOWED_ROOTS: Record<string, readonly string[]> = {
  design: ['design/'],
  stats: ['design/', 'stats/'],
  gallery: ['design/', 'gallery/'],
}

function importedPaths(path: string, source: string): string[] {
  return [...source.matchAll(IMPORT)].map(([, specifier]) => specifier!).flatMap((specifier) => {
    if (specifier.startsWith('#design/')) return [specifier.slice(1)]
    if (specifier.startsWith('.')) return [posix.join(posix.dirname(path), specifier)]
    return []
  })
}

describe('SFC policy', () => {
  it('finds every single file component', () => {
    expect(files.length).toBeGreaterThan(30)
  })

  it('flags the instance globals it forbids', () => {
    const [, globals] = templateRules[0]!
    expect(["$emit('x')", 'x($attrs)', '{{ $slots.default }}'].every((sample) => globals.test(sample))).toBe(true)
    expect(["$event", "'$name'", '`${emit}`'].some((sample) => globals.test(sample))).toBe(false)
  })

  it.each(files)('$path is a Vapor <script setup> with a pug template', ({ descriptor }) => {
    expect(descriptor.scriptSetup?.attrs.vapor).toBe(true)
    expect(descriptor.script).toBeNull()
    expect(descriptor.template?.lang).toBe('pug')
  })

  it.each(files)('$path template uses only Vapor-safe, CSP-safe syntax', ({ descriptor }) => {
    const template = descriptor.template?.content ?? ''
    const found = templateRules.filter(([, pattern]) => pattern.test(template)).map(([name]) => name)
    expect(found).toEqual([])
  })

  it.each(files)('$path script avoids APIs Vapor does not support', ({ descriptor }) => {
    const script = descriptor.scriptSetup?.content ?? ''
    const found = scriptRules.filter(([, pattern]) => pattern.test(script)).map(([name]) => name)
    expect(found).toEqual([])
  })

  it('has no TSX or JSX files: templates are Pug', () => {
    expect(nonStandardFiles).toEqual([])
  })

  it('flags the render functions it forbids', () => {
    const forbidden = scriptRules.slice(3).map(([, pattern]) => pattern)
    expect(['h(\'div\')', 'return { render: () => null }', 'defineComponent({})'].every((sample) => forbidden.some((rule) => rule.test(sample)))).toBe(true)
    expect(['search(x)', 'defineVaporComponent(() => 1)', 'rendered: true'].some((sample) => forbidden.some((rule) => rule.test(sample)))).toBe(false)
  })
})

describe('design primitives', () => {
  it('flags every raw form, table and dialog element in a Pug template', () => {
    const samples = ['button.ox-btn(type="button")', '  input(type="text")', '  select#theme', 'table.ox-table', 'dialog', '  p text #[button x]']
    expect(samples.map(rawElements)).toEqual([1, 1, 1, 1, 1, 1])
    expect(['Button(small)', '  p button', '  Table(caption="x")', '  .ox-buttons', '  | button'].map(rawElements)).toEqual([0, 0, 0, 0, 0])
  })

  it.each(files.filter(({ path }) => /^\.\/(stats|gallery)\//.test(path)))('$path builds on primitives, not raw elements', ({ descriptor }) => {
    expect(rawElements(descriptor.template?.content ?? '')).toBe(0)
  })

  it('never gains a raw element in a console template (the baseline only shrinks)', () => {
    const baseline: Record<string, number> = rawElementBaseline
    const consoleFiles = files.filter(({ path }) => !/^\.\/(design|stats|gallery)\//.test(path))
    const counts = Object.fromEntries(consoleFiles.map(({ path, descriptor }) => [path.slice(2), rawElements(descriptor.template?.content ?? '')]))
    const grown = Object.entries(counts).filter(([path, count]) => count > (baseline[path] ?? 0)).map(([path]) => path)
    expect(grown, 'use a primitive from src/design instead of a raw element').toEqual([])
    const stale = Object.keys(baseline).filter((path) => !counts[path])
    expect(stale, 'remove the entry from sfcPolicy.baseline.json: the file no longer has a raw element').toEqual([])
  })

  it.each(files.filter(({ path }) => /^\.\/(design|stats|gallery)\//.test(path)))('$path has no <style> block: its CSS is imported from a file', ({ descriptor }) => {
    expect(descriptor.styles).toEqual([])
  })
})

describe('import boundaries', () => {
  it('finds the imports of a source file', () => {
    const found = importedPaths('stats/a/B.vue', "import x from '../../api/y'\nimport '#design/format'\nimport { z } from './z'\nimport v from 'vue'")
    expect(found).toEqual(['api/y', 'design/format', 'stats/a/z'])
  })

  it.each(Object.keys(ALLOWED_ROOTS))('%s imports nothing from the console, the API or another page', (area) => {
    const offenders = ownedFiles
      .filter(({ path }) => path.startsWith(`${area}/`))
      .flatMap(({ path, source }) => importedPaths(path, source).map((target) => [path, target] as const))
      .filter(([path, target]) => !ALLOWED_ROOTS[area]!.some((root) => target.startsWith(root)) && !(TEST_SUPPORT.test(path) && target.startsWith('test/')))
    expect(offenders).toEqual([])
  })
})
