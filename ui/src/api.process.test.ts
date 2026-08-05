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
const serverBinary = join(targetDirectory, 'debug', 'oxiroute')

let child: ChildProcessWithoutNullStreams | undefined
let directory = ''
let listenerPath = ''
let origin = ''
let stderr = ''

beforeAll(async () => {
  const build = spawnSync('cargo', ['+1.87.0', 'build', '-p', 'oxiroute', '--bin', 'oxiroute'], {
    cwd: workspaceRoot,
    encoding: 'utf8',
    env: { ...process.env, CARGO_INCREMENTAL: '0' },
  })
  if (build.status !== 0) {
    throw new Error(`server build failed:\n${build.stdout}\n${build.stderr}`)
  }

  const port = await reservePort()
  directory = await mkdtemp(join(tmpdir(), 'oxiroute-ui-process-'))
  const configPath = join(directory, 'oxiroute.lua')
  const tokenPath = join(directory, 'management.token')
  listenerPath = join(directory, 'live.sock')
  await writeFile(configPath, managementConfig(port, listenerPath), 'utf8')
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
  await waitForServer(`${origin}/ready`)

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
      fetchMonitoring(undefined, token),
      fetchRtmpCatalog(undefined, token),
      fetchTopology(undefined, token),
      fetchConfig(token),
    ])

    expect(monitoring.listeners).toEqual([{
      administrativeState: 'ready',
      name: 'process-live',
      protocol: 'rtmp',
      bind: `unix:${listenerPath}`,
      maxConnections: 8,
      proxyProtocol: null,
      state: 'listening',
      acceptedConnections: '0',
      rejectedConnections: '0',
      activeConnections: 0,
      bytesReceived: '0',
      bytesSent: '0',
      httpOperations: null,
      tcpRelays: null,
      cache: null,
    }])
    expect(monitoring.upstreamPools).toEqual([])
    expect(monitoring.certbotCertificates).toEqual([])
    expect(monitoring.certbotWatcher).toBeNull()
    expect(monitoring.rtmp).toEqual({
      activeStreams: 0,
      publishers: 0,
      subscribers: 0,
      mediaPayloadBytesReceived: '0',
      recordingSupported: false,
      manualRecording: false,
      recorderBytesWritten: '0',
      recorderSegmentsStarted: '0',
      recorderSegmentsCompleted: '0',
      recorderDiscontinuities: '0',
      relayConnectionAttempts: '0',
      relayConnections: '0',
      relayReconnects: '0',
      relayEventsSent: '0',
      relayEventsDropped: '0',
      relayPayloadBytesSent: '0',
      relays: [],
      recorders: [],
    })
    expect(catalog).toEqual({
      revision: '0',
      as_of_unix_ms: expect.any(Number),
      capabilities: { live_ingest: true, manual_recording: false },
      streams: [],
    })
    expect(topology).toEqual({
      schemaVersion: 1,
      state: {
        config: 'active',
        runtime: 'active',
        sampledAtUnixMs: expect.any(Number),
      },
      nodes: [processTopologyNode(listenerPath)],
      edges: [],
      overlays: [{
        nodeId: 'rtmp_listener:12:process-live',
        state: 'listening',
        metrics: {
          activeConnections: 0,
          acceptedConnections: '0',
          rejectedConnections: '0',
          bytesReceived: '0',
          bytesSent: '0',
        },
      }],
    })
    expect(snapshot).toEqual({
      schemaVersion: 1,
      diskRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      candidateRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      activeRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      config: {
        version: 1,
        max_connections: null,
        management: { bind: origin.slice('http://'.length), ui_dir: null },
        stats: null,
        certificates: [],
        tls_profiles: [],
        listeners: [{
          name: 'process-live',
          bind: { type: 'unix', path: listenerPath, mode: null },
          protocol: 'rtmp',
          service: 'live',
          tls_profile: null,
          proxy_protocol: null,
          max_connections: 8,
          downstream_timeouts: {
            client_timeout_ms: null,
            request_timeout_ms: null,
            keepalive_timeout_ms: null,
          },
        }],
        cache_stores: [],
        upstream_pools: [],
        http_services: [],
        forward_proxy_services: [],
          rtmp_services: [{
            name: 'live',
            outbound_chunk_size: 4_096,
            access_log: null,
            outbound_policy: {
              allow_domains: [],
              deny_domains: [],
              allow_cidrs: [],
              deny_cidrs: [],
              deny_private: true,
              rtmps: 'disabled',
              max_chain_depth: 4,
            },
            callbacks: {
              on_connect: null,
              on_disconnect: null,
              on_publish: null,
              on_publish_done: null,
              on_play: null,
              on_play_done: null,
              on_done: null,
              on_update: null,
              notify_method: 'post',
              timeout_ms: 10_000,
              notify_update_timeout_ms: 30_000,
              notify_update_strict: false,
              notify_relay_redirect: false,
            },
            exec_profiles: [],
            applications: [{
            name: 'broadcast',
            live: true,
            idle_streams: false,
            publish: { rules: [], token: null },
            play: { rules: [], token: null },
              limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
              pull_targets: [],
              push_targets: [],
              relay: {
                max_queue_messages: 256,
                max_queue_bytes: 8_388_608,
                buffer_ms: 5_000,
                push_reconnect_ms: 3_000,
                pull_reconnect_ms: 3_000,
                connect_timeout_ms: 500,
                handshake_timeout_ms: 2_000,
              },
              callbacks: {
                on_connect: null,
                on_disconnect: null,
                on_publish: null,
                on_publish_done: null,
                on_play: null,
                on_play_done: null,
                on_done: null,
                on_update: null,
                notify_method: 'post',
                timeout_ms: 10_000,
                notify_update_timeout_ms: 30_000,
                notify_update_strict: false,
                notify_relay_redirect: false,
              },
              dash: null,
              fanout: {
              max_subscribers: 1_024,
              max_queue_messages_per_subscriber: 256,
                max_queue_bytes_per_subscriber: 8_388_608,
              },
              hls: null,
              vod: null,
              recorders: [],
          }],
        }],
        l4_services: [],
      },
      configFormat: 'lua',
      compositional: false,
      dependencyCount: 0,
      configPreview: expect.stringContaining('return {'),
      luaPreview: expect.stringContaining('return {'),
      diagnostics: [],
    })
    expect(snapshot).not.toHaveProperty('dependencies')
    expect(snapshot).not.toHaveProperty('sourcePath')

    const validation = await validateConfig(snapshot.config, token)
    expect(validation.normalizedConfig).toEqual(snapshot.config)
    expect(validation.restartRequired).toBe(false)
    expect(validation.configFormat).toBe('lua')
    expect(validation.compositional).toBe(false)
    expect(validation.dependencyCount).toBe(0)
    expect(validation.configPreview).toContain('return {')
    expect(validation.luaPreview).toBe(validation.configPreview)
    expect(validation.diagnostics).toEqual([])
    expect(validation.topology).toEqual({
      schemaVersion: 1,
      state: {
        config: 'candidate',
        runtime: 'not_active',
        sampledAtUnixMs: expect.any(Number),
      },
      nodes: [processTopologyNode(listenerPath)],
      edges: [],
      overlays: [],
    })
    expect(validation).not.toHaveProperty('dependencies')
    expect(validation).not.toHaveProperty('sourcePath')

    const saved = await saveConfig(snapshot.config, snapshot.diskRevision, token)
    expect(saved).toEqual({
      diskRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      candidateRevision: expect.stringMatching(/^[0-9a-f]{64}$/),
      activeRevision: snapshot.activeRevision,
      outcome: 'unchanged_active',
      activationState: 'active',
      restartRequired: false,
      diagnostics: [],
    })
    expect(saved.diskRevision).not.toBe(snapshot.diskRevision)
    expect(await fetchConfig(token)).toEqual({
      ...snapshot,
      diskRevision: saved.diskRevision,
      candidateRevision: saved.candidateRevision,
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

function managementConfig(port: number, listenerPath: string): string {
  return `return {
  version = 1,
  management = {
    bind = "127.0.0.1:${port}",
    ui_dir = nil,
  },
  certificates = {},
  tls_profiles = {},
  listeners = {
    {
      name = "process-live",
      bind = { type = "unix", path = "${listenerPath}" },
      protocol = "rtmp",
      service = "live",
      max_connections = 8,
    },
  },
  cache_stores = {},
  upstream_pools = {},
  http_services = {},
  forward_proxy_services = {},
  rtmp_services = {
    {
      name = "live",
      applications = {
        { name = "broadcast", live = true, idle_streams = false, recorders = {} },
      },
    },
  },
  l4_services = {},
}\n`
}

function processTopologyNode(path: string) {
  return {
    id: 'rtmp_listener:12:process-live',
    kind: 'rtmp_listener',
    name: 'process-live',
    configPath: '/listeners/0',
    attributes: {
      bind: { type: 'unix', path, mode: null },
      downstreamTimeouts: {
        clientTimeoutMs: null,
        keepaliveTimeoutMs: null,
        requestTimeoutMs: null,
      },
      protocol: 'rtmp',
      service: 'live',
      tlsProfile: null,
      maxConnections: 8,
      outboundChunkSize: 4_096,
      accessLog: 'default_disabled',
      applications: [{
        name: 'broadcast',
        live: true,
        idleStreams: false,
        pushTargetCount: 0,
        fanout: {
          maxSubscribers: 1_024,
          maxQueueMessagesPerSubscriber: 256,
          maxQueueBytesPerSubscriber: 8_388_608,
        },
        recording: {
          supported: false,
          recorderCount: 0,
          manualRecorderCount: 0,
          continuousRecorderCount: 0,
        },
      }],
    },
  }
}
