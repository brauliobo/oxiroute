import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { isAbsolute, join, resolve } from 'node:path'

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

import {
  ApiError,
  fetchConfig,
  fetchMonitoring,
  fetchRtmpCatalog,
  fetchTopology,
  saveConfig,
  validateConfig,
} from './api'

const token = '4d56dcfbb90f5666c794cc8f23c6758e7bb388c7b1847ea385a75ce53647bbac'
const workspaceRoot = resolve(process.cwd(), '..')
const targetDirectory = process.env.CARGO_TARGET_DIR
  ? (isAbsolute(process.env.CARGO_TARGET_DIR)
      ? process.env.CARGO_TARGET_DIR
      : resolve(workspaceRoot, process.env.CARGO_TARGET_DIR))
  : join(workspaceRoot, 'target')
const serverBinary = join(targetDirectory, 'debug', 'oxiroute-server')

let child: ChildProcessWithoutNullStreams | undefined
let directory = ''
let origin = ''
let stderr = ''

beforeAll(async () => {
  const build = spawnSync('cargo', ['build', '-p', 'oxiroute-server', '--bin', 'oxiroute-server'], {
    cwd: workspaceRoot,
    encoding: 'utf8',
  })
  if (build.status !== 0) {
    throw new Error(`server build failed:\n${build.stdout}\n${build.stderr}`)
  }

  const port = await reservePort()
  directory = await mkdtemp(join(tmpdir(), 'oxiroute-ui-process-'))
  const configPath = join(directory, 'oxiroute.lua')
  const tokenPath = join(directory, 'management.token')
  await writeFile(configPath, emptyManagementConfig(port), 'utf8')
  await writeFile(tokenPath, `${token}\n`, 'utf8')
  await chmod(tokenPath, 0o600)

  child = spawn(serverBinary, [configPath], {
    cwd: workspaceRoot,
    env: {
      ...process.env,
      OXIROUTE_MANAGEMENT_TOKEN_FILE: tokenPath,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', (chunk: string) => {
    stderr += chunk
  })
  origin = `http://127.0.0.1:${port}`
  await waitForServer(`${origin}/api/v1/monitoring`)

  const nativeFetch = globalThis.fetch
  vi.stubGlobal('fetch', (input: RequestInfo | URL, init?: RequestInit) =>
    nativeFetch(new URL(String(input), origin), init),
  )
}, 180_000)

afterAll(async () => {
  vi.unstubAllGlobals()
  if (child && child.exitCode === null) {
    child.kill('SIGTERM')
    await new Promise<void>((resolveExit) => {
      const force = setTimeout(() => {
        child?.kill('SIGKILL')
      }, 2_000)
      child?.once('exit', () => {
        clearTimeout(force)
        resolveExit()
      })
    })
  }
  if (directory) await rm(directory, { force: true, recursive: true })
})

describe('production API client against the built management process', () => {
  it('runs the browser fetch sequence against real response JSON and token authentication', async () => {
    const [monitoring, catalog, topology, snapshot] = await Promise.all([
      fetchMonitoring(),
      fetchRtmpCatalog(),
      fetchTopology(),
      fetchConfig(token),
    ])

    expect(monitoring.listeners).toEqual([])
    expect(monitoring.upstreamPools).toEqual([])
    expect(monitoring.certbotCertificates).toEqual([])
    expect(monitoring.certbotWatcher).toBeNull()
    expect(monitoring.rtmp).toEqual({
      activeStreams: 0,
      publishers: 0,
      subscribers: 0,
      mediaPayloadBytesReceived: 0,
      recordingSupported: false,
      manualRecording: false,
      recorderBytesWritten: 0,
      recorderSegmentsStarted: 0,
      recorderSegmentsCompleted: 0,
      recorderDiscontinuities: 0,
      recorders: [],
    })
    expect(catalog).toEqual({
      revision: '0',
      as_of_unix_ms: expect.any(Number),
      capabilities: { live_ingest: false, manual_recording: false },
      streams: [],
    })
    expect(topology).toEqual({
      schemaVersion: 1,
      state: {
        config: 'active',
        runtime: 'active',
        sampledAtUnixMs: expect.any(Number),
      },
      nodes: [],
      edges: [],
      overlays: [],
    })
    expect(snapshot).toEqual({
      schemaVersion: 1,
      diskRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      activeRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      config: {
        version: 1,
        management: { bind: origin.slice('http://'.length), ui_dir: null },
        certificates: [],
        tls_profiles: [],
        listeners: [],
        cache_stores: [],
        upstream_pools: [],
        http_services: [],
        forward_proxy_services: [],
        rtmp_services: [],
        l4_services: [],
      },
      diagnostics: [],
    })

    const validation = await validateConfig(snapshot.config, token)
    expect(validation.normalizedConfig).toEqual(snapshot.config)
    expect(validation.diagnostics).toEqual([])
    expect(validation.topology).toEqual({
      schemaVersion: 1,
      state: {
        config: 'candidate',
        runtime: 'not_active',
        sampledAtUnixMs: expect.any(Number),
      },
      nodes: [],
      edges: [],
      overlays: [],
    })
    expect(validation.luaPreview).toContain('return {')

    const saved = await saveConfig(snapshot.config, snapshot.diskRevision, token)
    expect(saved).toEqual({
      diskRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      activeRevision: snapshot.activeRevision,
      outcome: 'saved_restart_required',
      activationState: 'restart_required',
      restartRequired: true,
      diagnostics: [{
        code: 'W_RESTART_REQUIRED',
        message: 'configuration was saved; restart the daemon to activate it',
        severity: 'warning',
        stage: 'activation',
      }],
    })
    expect(saved.diskRevision).not.toBe(snapshot.diskRevision)
    expect(await fetchConfig(token)).toEqual({
      ...snapshot,
      diskRevision: saved.diskRevision,
    })

    try {
      await fetchConfig('wrong-token')
      expect.unreachable('wrong management token was accepted')
    } catch (error) {
      expect(error).toBeInstanceOf(ApiError)
      expect(error).toMatchObject({ name: 'ApiError', status: 401 })
    }
  })
})

async function reservePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((resolveListen, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolveListen)
  })
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('could not reserve management port')
  await new Promise<void>((resolveClose, reject) => {
    server.close((error) => error ? reject(error) : resolveClose())
  })
  return address.port
}

async function waitForServer(url: string): Promise<void> {
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline) {
    if (child?.exitCode !== null) {
      throw new Error(`server exited before binding the management socket:\n${stderr}`)
    }
    try {
      const response = await fetch(url)
      if (response.ok) return
    } catch {
      // The built process has not bound its management listener yet.
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 10))
  }
  throw new Error(`server did not bind the management socket:\n${stderr}`)
}

function emptyManagementConfig(port: number): string {
  return `return {
  version = 1,
  management = {
    bind = "127.0.0.1:${port}",
    ui_dir = nil,
  },
  certificates = {},
  tls_profiles = {},
  listeners = {},
  cache_stores = {},
  upstream_pools = {},
  http_services = {},
  forward_proxy_services = {},
  rtmp_services = {},
  l4_services = {},
}\n`
}
