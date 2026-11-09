import { parse } from 'vue/compiler-sfc'
import { describe, expect, it } from 'vitest'

// Every SFC is Vapor, so templates may only use what the Vapor production compiler supports. `$emit` and friends
// compile in development and in tests but break the production bundle at runtime, so they are checked on the source.
const sources = import.meta.glob<string>('./**/*.vue', { eager: true, query: '?raw', import: 'default' })
const files = Object.entries(sources).map(([path, source]) => ({ path, descriptor: parse(source).descriptor }))

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
]

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
})
