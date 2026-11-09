import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { randomBytes } from 'node:crypto'
import { afterEach, describe, expect, it } from 'vitest'

// The size budget script runs against hand-made output directories.
const script = join(import.meta.dirname, '../../../scripts/verify-stats-budget.mjs')
const made: string[] = []
afterEach(() => made.splice(0).forEach((directory) => rmSync(directory, { recursive: true, force: true })))

function build(files: Record<string, string | Buffer>, manifest: object | undefined = {
  'stats.html': { file: 'assets/stats-abc.js', src: 'stats.html', isEntry: true, css: ['assets/stats-def.css'] },
}) {
  const directory = mkdtempSync(join(tmpdir(), 'stats-budget-'))
  made.push(directory)
  mkdirSync(join(directory, 'assets'))
  writeFileSync(join(directory, 'stats.html'), '<!doctype html>')
  if (manifest) writeFileSync(join(directory, 'manifest.json'), JSON.stringify(manifest))
  for (const [name, content] of Object.entries(files)) writeFileSync(join(directory, name), content)
  return directory
}
const run = (directory: string) => {
  try {
    return { code: 0, output: execFileSync('node', [script, directory], { encoding: 'utf8', stdio: 'pipe' }) }
  } catch (error) {
    const failure = error as { status: number; stderr: string }
    return { code: failure.status, output: failure.stderr }
  }
}
const good = { 'assets/stats-abc.js': 'console.log(1)', 'assets/stats-def.css': 'body{}' }

describe('verify-stats-budget', () => {
  it('accepts a small bundle that matches its manifest', () => {
    const result = run(build(good))
    expect(result.code).toBe(0)
    expect(result.output).toContain('within budget')
  })

  it('rejects a script over the gzip budget (incompressible bytes)', () => {
    const result = run(build({ ...good, 'assets/stats-abc.js': randomBytes(61 * 1024) }))
    expect(result.code).toBe(1)
    expect(result.output).toContain('js is')
  })

  it('rejects a stylesheet over its budget', () => {
    expect(run(build({ ...good, 'assets/stats-def.css': randomBytes(13 * 1024) })).output).toContain('css is')
  })

  it('rejects files it does not expect: images, fonts, maps, other names', () => {
    for (const extra of ['assets/logo.svg', 'assets/stats-abc.js.map', 'assets/stats-font.woff2']) {
      expect(run(build({ ...good, [extra]: 'x' })).output).toContain('unexpected asset')
    }
  })

  it('rejects a manifest that does not match the assets', () => {
    expect(run(build({ 'assets/stats-abc.js': 'x' })).output).toContain('missing file')
    const other = { 'stats.html': { file: 'assets/stats-zzz.js', src: 'stats.html', isEntry: true } }
    expect(run(build(good, other)).output).toContain('missing file')
  })
})
