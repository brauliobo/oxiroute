import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { renderTypes } from './lib/openapi-types.mjs'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const inputPath = path.join(root, 'contracts', 'stats-page.openapi.json')
const outputPath = path.join(root, 'ui', 'src', 'stats', 'generated', 'statsPage.ts')
const COUNTER = 'DecimalCounter'
const memo = new Map()

const document = JSON.parse(await readFile(inputPath, 'utf8'))
const schemas = document.components?.schemas
const generated = renderTypes(document, {
  generator: 'scripts/generate-stats-page-types.mjs',
  prefix: 'StatsPage',
  decimalSchemas: [COUNTER],
}) + renderShape()

if (process.argv.includes('--check')) {
  const existing = await readFile(outputPath, 'utf8').catch(() => null)
  if (existing !== generated) {
    throw new Error(`Generated stats page types are stale: ${outputPath}`)
  }
} else {
  await mkdir(path.dirname(outputPath), { recursive: true })
  await writeFile(outputPath, generated)
}

// Decimal-string counters are strings on the wire and indistinguishable from names by value, so the decoder walks
// this table: per schema, the properties that hold a counter (`c`), a list of counters (`c[]`) or a value that
// contains counters (`Schema`, `Schema[]`). Schemas and properties without counters are left out.
function renderShape() {
  const names = Object.keys(schemas).sort().filter((name) => Object.keys(propertyKinds(name)).length > 0)
  const lines = [
    '/** Where the decimal-string counters of each schema are; see scripts/generate-stats-page-types.mjs. */',
    'export const statsPageCounterShape: Readonly<Record<string, Readonly<Record<string, string>>>> = {',
  ]
  for (const name of names) {
    const entries = Object.entries(propertyKinds(name)).map(([property, kind]) => `${/^[A-Za-z_$][\w$]*$/.test(property) ? property : JSON.stringify(property)}: '${kind}'`)
    lines.push(`  ${name}: { ${entries.join(', ')} },`)
  }
  lines.push('}', '')
  return lines.join('\n')
}

function propertyKinds(name) {
  if (memo.has(name)) return memo.get(name)
  memo.set(name, {})
  const kinds = {}
  for (const [property, schema] of Object.entries(schemas[name].properties ?? {})) {
    const kind = valueKind(schema)
    if (kind) kinds[property] = kind
  }
  memo.set(name, kinds)
  return kinds
}

function valueKind(schema) {
  if (schema.type === 'array') {
    const item = valueKind(schema.items)
    return item && `${item}[]`
  }
  const target = schema.$ref?.split('/').at(-1)
  if (!target) return null
  if (target === COUNTER) return 'c'
  return Object.keys(propertyKinds(target)).length > 0 ? target : null
}
