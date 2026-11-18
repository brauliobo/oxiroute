import { spawn } from 'node:child_process'
import type { ChildProcessWithoutNullStreams } from 'node:child_process'
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'

export const MANAGEMENT_TOKEN = '4d56dcfbb90f5666c794cc8f23c6758e7bb388c7b1847ea385a75ce53647bbac'

const workspaceRoot = resolve(process.cwd(), '..')
const targetDirectory = process.env.CARGO_TARGET_DIR
  ? (isAbsolute(process.env.CARGO_TARGET_DIR)
      ? process.env.CARGO_TARGET_DIR
      : resolve(workspaceRoot, process.env.CARGO_TARGET_DIR))
  : join(workspaceRoot, 'target')
const serverBinary = join(targetDirectory, 'debug', 'oxiroute')

export interface ManagementProcess {
  origin: string
  directory: string
  token: string
  stderr: () => string
  stop: () => Promise<void>
}

export interface Layout {
  directory: string
  port: number
}

async function reservePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((resolveListen, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolveListen)
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('could not reserve management port')
  await new Promise<void>((resolveClose, reject) => server.close((error) => error ? reject(error) : resolveClose()))
  return address.port
}

const answers = (url: string): Promise<boolean> => fetch(url).then((response) => response.ok, () => false)

async function waitForServer(url: string, child: ChildProcessWithoutNullStreams, stderr: () => string): Promise<void> {
  const deadline = Date.now() + 40_000
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`server exited before binding the management socket:\n${stderr()}`)
    if (await answers(url)) return
    await new Promise((resolveWait) => setTimeout(resolveWait, 10))
  }
  throw new Error(`server did not bind the management socket:\n${stderr()}`)
}

async function terminate(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.exitCode !== null) return
  child.kill('SIGTERM')
  await new Promise<void>((resolveExit) => {
    const force = setTimeout(() => child.kill('SIGKILL'), 2_000)
    child.once('exit', () => {
      clearTimeout(force)
      resolveExit()
    })
  })
}

export async function startManagement(
  config: (layout: Layout) => string,
  prepare: (layout: Layout) => Promise<void> = async () => undefined,
): Promise<ManagementProcess> {
  const port = await reservePort()
  const directory = await mkdtemp(join(tmpdir(), 'oxiroute-ui-process-'))
  const configPath = join(directory, 'oxiroute.lua')
  const tokenPath = join(directory, 'management.token')
  await prepare({ directory, port })
  await writeFile(configPath, config({ directory, port }), 'utf8')
  await writeFile(tokenPath, `${MANAGEMENT_TOKEN}\n`, 'utf8')
  await chmod(tokenPath, 0o600)

  const child = spawn(serverBinary, [configPath], {
    cwd: workspaceRoot,
    env: {
      ...process.env,
      OXIROUTE_AUDIT_DIR: join(directory, 'audit'),
      OXIROUTE_AUDIT_MAX_FILE_BYTES: '1048576',
      OXIROUTE_AUDIT_MAX_RECORD_BYTES: '16384',
      OXIROUTE_AUDIT_MAX_RECORDS: '10000',
      OXIROUTE_AUDIT_MAX_ROTATED_FILES: '7',
      OXIROUTE_AUDIT_MAX_TOTAL_BYTES: '8388608',
      OXIROUTE_MANAGEMENT_TOKEN_FILE: tokenPath,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  let stderr = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', (chunk: string) => {
    stderr += chunk
  })
  const origin = `http://127.0.0.1:${port}`
  const stop = async () => {
    await terminate(child)
    await rm(directory, { force: true, recursive: true })
  }
  try {
    await waitForServer(`${origin}/ready`, child, () => stderr)
  } catch (error) {
    await stop()
    throw error
  }
  return { origin, directory, token: MANAGEMENT_TOKEN, stderr: () => stderr, stop }
}
