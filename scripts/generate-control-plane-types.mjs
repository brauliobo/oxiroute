import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { renderTypes } from './lib/openapi-types.mjs'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const inputPath = path.join(root, 'contracts', 'control-plane.openapi.json')
const outputPath = path.join(root, 'ui', 'src', 'api', 'generated', 'controlPlane.ts')

const document = JSON.parse(await readFile(inputPath, 'utf8'))
const generated = renderTypes(document, { generator: 'scripts/generate-control-plane-types.mjs', prefix: 'ControlPlane' })

if (process.argv.includes('--check')) {
  const existing = await readFile(outputPath, 'utf8').catch(() => null)
  if (existing !== generated) {
    throw new Error(`Generated control-plane types are stale: ${outputPath}`)
  }
} else {
  await mkdir(path.dirname(outputPath), { recursive: true })
  await writeFile(outputPath, generated)
}
