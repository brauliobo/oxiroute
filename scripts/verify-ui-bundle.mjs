#!/usr/bin/env node
// Fails unless every JS asset of the given built UI directories is a pure Vapor bundle: the VDOM renderer (its
// `__v_isVNode` marker survives minification) must be absent and the Vapor runtime (`__vapor`) present.
// usage: node scripts/verify-ui-bundle.mjs <dist-dir>...
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

const directories = process.argv.slice(2)
if (directories.length === 0) throw new Error('usage: verify-ui-bundle.mjs <dist-dir>...')

const problems = []
for (const directory of directories) {
  const assets = join(directory, 'assets')
  const scripts = readdirSync(assets).filter((name) => name.endsWith('.js'))
  if (scripts.length === 0) problems.push(`${directory}: no JS assets`)
  for (const name of scripts) {
    const source = readFileSync(join(assets, name), 'utf8')
    if (source.includes('__v_isVNode')) problems.push(`${directory}/assets/${name}: contains the VDOM renderer`)
    if (!source.includes('__vapor')) problems.push(`${directory}/assets/${name}: does not contain the Vapor runtime`)
  }
}
if (problems.length > 0) {
  console.error(problems.join('\n'))
  process.exit(1)
}
console.log(`pure Vapor bundle: ${directories.join(', ')}`)
