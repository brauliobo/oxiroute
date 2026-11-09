#!/usr/bin/env node
// Fails unless the built stats page (ui/dist-stats) is what the Rust side embeds and fits the size budget:
//  - only `stats.html`, `manifest.json` and `assets/stats-<hash>.js|css` (no fonts, images or source maps),
//  - the manifest names the entry script and its stylesheets, and those files exist,
//  - gzip (level 9) of all JS <= 90 KiB and of all CSS <= 12 KiB.
// usage: node scripts/verify-stats-budget.mjs <dist-dir>
import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'

export const BUDGET_BYTES = { js: 90 * 1024, css: 12 * 1024 }
const ASSET = /^stats-[A-Za-z0-9_-]+\.(js|css)$/

/** @returns {{ problems: string[], sizes: Record<string, number> }} gzip bytes by type */
export function checkStatsBundle(directory) {
  const problems = []
  const sizes = { js: 0, css: 0 }
  const top = readdirSync(directory).sort()
  for (const name of top) if (!['assets', 'manifest.json', 'stats.html'].includes(name)) problems.push(`unexpected file: ${name}`)
  const assets = readdirSync(join(directory, 'assets')).sort()
  for (const name of assets) {
    const match = ASSET.exec(name)
    if (!match) problems.push(`unexpected asset: assets/${name}`)
    else sizes[match[1]] += gzipSync(readFileSync(join(directory, 'assets', name)), { level: 9 }).length
  }
  const entry = JSON.parse(readFileSync(join(directory, 'manifest.json'), 'utf8'))['stats.html']
  const listed = [entry?.file, ...(entry?.css ?? [])]
  if (!entry?.isEntry || !entry.file?.endsWith('.js')) problems.push('manifest has no entry script for stats.html')
  for (const file of listed) if (!assets.includes(String(file).replace(/^assets\//, ''))) problems.push(`manifest names a missing file: ${file}`)
  if (assets.length !== listed.length) problems.push(`assets (${assets.length}) differ from the manifest (${listed.length})`)
  for (const [type, limit] of Object.entries(BUDGET_BYTES)) {
    if (sizes[type] > limit) problems.push(`${type} is ${sizes[type]} bytes gzip, over the budget of ${limit}`)
  }
  return { problems, sizes }
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const directory = process.argv[2]
  if (!directory) throw new Error('usage: verify-stats-budget.mjs <dist-dir>')
  const { problems, sizes } = checkStatsBundle(directory)
  if (problems.length > 0) {
    console.error(problems.join('\n'))
    process.exit(1)
  }
  const kib = (bytes) => `${(bytes / 1024).toFixed(1)} KiB`
  console.log(`stats bundle within budget: js ${kib(sizes.js)} of ${kib(BUDGET_BYTES.js)} gzip, css ${kib(sizes.css)} of ${kib(BUDGET_BYTES.css)} gzip`)
}
