import { createHash } from 'node:crypto'
import { mkdir } from 'node:fs/promises'
import { join } from 'node:path'

import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'

import {
  ApiError,
  createMediaTicket,
  createRecordingTicket,
  deleteRecording,
  fetchRecordings,
  fetchRtmpCatalog,
  fetchRtmpStats,
  fetchSecretFingerprints,
} from './api'
import type { Recording, RecordingPage } from './api'
import { MANAGEMENT_TOKEN as token, startManagement } from './test/managementProcess'
import type { Layout, ManagementProcess } from './test/managementProcess'
import { RtmpPublisher } from './test/rtmpPublisher'

const PUBLISH_KEY = 'publish-key-for-tests-only'
const PLAY_KEY = 'play-key-for-tests-only'
const STREAM = 'cam-front'
const FRAMES = 40
const FRAME_MS = 100
const SPS = Buffer.from('6742c00bdc1419f9f011000003000100000300140f142b80', 'hex')
const PPS = Buffer.from('68ce0194b2', 'hex')
const IDR = Buffer.from(`65888404bc9b93${'93'.repeat(13)}${'aeba'.repeat(76)}c0`, 'hex')

const fingerprint = (secret: string) => `fp-${createHash('sha256').update(secret).digest('hex').slice(0, 8)}`
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

let management: ManagementProcess
let origin = ''
let publisher: RtmpPublisher
let publishing: Promise<void>

const socketPath = (layout: Layout) => join(layout.directory, 'ingest.sock')

function config(layout: Layout): string {
  const { directory, port } = layout
  const policy = (secret: string) => `{ token = { source = "stream_query", parameter = "token", secret = "${secret}" } }`
  return `return {
  version = 1,
  management = { bind = "127.0.0.1:${port}", ui_dir = nil },
  certificates = {},
  tls_profiles = {},
  listeners = {
    { name = "ingest", bind = { type = "unix", path = "${socketPath(layout)}" }, protocol = "rtmp", service = "edge", max_connections = 8 },
  },
  cache_stores = {},
  upstream_pools = {},
  http_services = {},
  forward_proxy_services = {},
  rtmp_services = {
    {
      name = "edge",
      applications = {
        {
          name = "live",
          live = true,
          idle_streams = false,
          publish = ${policy(PUBLISH_KEY)},
          play = ${policy(PLAY_KEY)},
          hls = { root_directory = "${directory}/hls", segment_duration_ms = 1000 },
          recorders = {
            {
              name = "archive",
              start = "continuous",
              root_directory = "${directory}/recordings",
              max_frames = 6,
              max_storage_files = 4,
              retention = "delete_oldest",
              retention_max_age_ms = 604800000,
            },
          },
        },
      },
    },
  },
  l4_services = {},
}\n`
}

async function stream(): Promise<void> {
  publisher.metadata({ width: 320, height: 180, framerate: 10, videocodecid: 7, audiocodecid: 10 })
  publisher.avcHeaders({ sps: SPS, pps: PPS }, 0)
  publisher.aacHeader(0)
  for (let frame = 1; frame <= FRAMES; frame++) {
    publisher.keyframe(IDR, frame * FRAME_MS)
    publisher.aacFrame(frame * FRAME_MS)
    await pause(FRAME_MS)
  }
}

async function until<T>(read: () => Promise<T>, done: (value: T) => boolean, what: string): Promise<T> {
  const deadline = Date.now() + 15_000
  for (;;) {
    const value = await read()
    if (done(value)) return value
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await pause(100)
  }
}

const raw = (path: string, headers: Record<string, string> = {}) => fetch(path, { headers })

beforeAll(async () => {
  management = await startManagement(config, async ({ directory }) => {
    await Promise.all(['recordings', 'hls'].map((name) => mkdir(join(directory, name), { mode: 0o700 })))
  })
  origin = management.origin
  const nativeFetch = globalThis.fetch
  vi.stubGlobal('fetch', (input: RequestInfo | URL, init?: RequestInit) => nativeFetch(new URL(String(input), origin), init))
  publisher = await RtmpPublisher.connect(socketPath({ directory: management.directory, port: 0 }), 'live')
  expect(await publisher.publish(`${STREAM}?token=${PUBLISH_KEY}`)).toBe('NetStream.Publish.Start')
  publishing = stream()
}, 60_000)

afterAll(async () => {
  vi.unstubAllGlobals()
  await publishing?.catch(() => undefined)
  await management?.stop()
})

describe('console API client against a real publisher, recorder and HLS output', { timeout: 30_000 }, () => {
  it('shows the camera live with the picture the publisher announced and its publisher session', async () => {
    const catalog = await until(() => fetchRtmpCatalog(token), ({ streams }) => streams.some(({ state }) => state === 'live'), 'a live stream')
    const live = catalog.streams.find(({ name }) => name === STREAM)!
    expect(live).toMatchObject({ server_id: 'edge', application: 'live', state: 'live' })
    expect(live.media.video.codec_name).toBe('avc')
    expect(live.media.audio.codec_name).toBe('aac')
    expect(live.media.profile).toMatchObject({ video_width: 320, video_height: 180 })
    expect(live.recorders.map(({ name, manual }) => [name, manual])).toEqual([['archive', false]])
    expect(catalog.recently_ended_streams).toEqual([])

    const stats = await fetchRtmpStats(token)
    expect(stats.clients).toEqual([expect.objectContaining({
      id: live.publisher!.session_id, role: 'publisher', application: 'live', stream: STREAM, service: 'edge',
    })])
  })

  it('lists the fingerprint of the publish and play keys and never the keys', async () => {
    const secrets = await fetchSecretFingerprints(token)
    expect(secrets).toEqual([
      { service: 'edge', application: 'live', role: 'play', fingerprint: fingerprint(PLAY_KEY) },
      { service: 'edge', application: 'live', role: 'publish', fingerprint: fingerprint(PUBLISH_KEY) },
    ])
    const response = await raw('/api/v1/rtmp/secrets', { Authorization: `Bearer ${token}` })
    const body = await response.text()
    expect([PUBLISH_KEY, PLAY_KEY].some((key) => body.includes(key))).toBe(false)
    expect(response.headers.get('cache-control')).toBe('no-store')
  })

  it('mints a live preview ticket that plays the HLS playlist without a bearer token and only for that stream', async () => {
    const ticket = await createMediaTicket('edge', 'live', STREAM, token)
    expect(ticket.ttlSeconds).toBe(600)
    expect(ticket.path).toMatch(new RegExp(`^/api/v1/rtmp/media/edge/live/${STREAM}/index\\.m3u8\\?ticket=`))
    const playlist = await until(() => raw(ticket.path), ({ status }) => status === 200, 'the HLS playlist')
    expect(await playlist.text()).toContain('#EXTM3U')
    expect(playlist.headers.get('referrer-policy')).toBe('no-referrer')
    expect(playlist.headers.get('cache-control')).toBe('no-store')

    expect((await raw(ticket.path.replace(STREAM, 'cam-other'))).status).toBe(401)
    expect((await raw(`/api/v1/rtmp/recordings?ticket=${ticket.ticket}`)).status).toBe(401)
    await expect(createMediaTicket('edge', 'nothing', STREAM, token)).rejects.toMatchObject({ status: 404, code: 'media_not_found' })
  })

  it('keeps the recorder within its file quota by deleting the oldest and reports the retention', async () => {
    await publishing
    const page = await until(() => fetchRecordings(token), ({ recordings }) => recordings.some(({ status }) => status === 'complete'), 'a finished recording')
    expect(page.recordings.length).toBeLessThanOrEqual(4)
    expect(page.recordings.filter(({ status }) => status === 'recording')).toHaveLength(1)
    expect(page.recordings.every(({ stream, audio_codec: audio }) => stream === STREAM && audio === 'aac')).toBe(true)
    expect(page.roots).toHaveLength(1)
    expect(page.roots[0]).toMatchObject({
      retention: 'delete_oldest', retention_max_age_ms: 604_800_000, max_files: 4, files: page.recordings.length,
      recorders: [{ service: 'edge', application: 'live', recorder: 'archive' }],
    })
    expect(Number(page.roots[0]!.evicted_files)).toBeGreaterThan(0)
    expect(Number(page.roots[0]!.bytes_used)).toBe(page.recordings.reduce((sum, { size_bytes: size }) => sum + Number(size), 0))
  })

  it('shows the camera offline with the reason once the publisher leaves, and keeps its last profile', async () => {
    await publisher.close()
    const catalog = await until(() => fetchRtmpCatalog(token), ({ recently_ended_streams: ended }) => ended.length > 0, 'the ended stream')
    expect(catalog.streams.find(({ name }) => name === STREAM)).toBeUndefined()
    expect(catalog.recently_ended_streams).toEqual([expect.objectContaining({
      name: STREAM, application: 'live', server_id: 'edge', state: 'offline', end_reason: 'closed',
    })])
    expect(catalog.recently_ended_streams[0]!.media.profile).toMatchObject({ video_width: 320, video_height: 180 })
    expect((await fetchRtmpStats(token)).clients).toEqual([])
  })

  describe('downloads and deletes', () => {
    let page: RecordingPage
    let target: Recording

    beforeAll(async () => {
      page = await fetchRecordings(token, { stream: STREAM, status: 'complete' })
      target = page.recordings[0]!
    })

    it('downloads a recording through a ticket with no bearer token, as an attachment, and refuses it on other routes', async () => {
      const ticket = await createRecordingTicket(target.id, token)
      expect(ticket.path).toBe(`/api/v1/rtmp/recordings/${target.id}/content?ticket=${ticket.ticket}`)
      const response = await raw(ticket.path)
      expect(response.status).toBe(200)
      expect(response.headers.get('content-disposition')).toMatch(/^attachment; filename="/)
      expect(response.headers.get('referrer-policy')).toBe('no-referrer')
      expect(response.headers.get('cache-control')).toBe('no-store')
      expect((await response.arrayBuffer()).byteLength).toBe(Number(target.size_bytes))

      expect((await raw(`/api/v1/rtmp/recordings/${target.id}?ticket=${ticket.ticket}`)).status).toBe(401)
      expect((await raw(`/api/v1/rtmp/recordings/${page.recordings[1]?.id ?? 'other'}/content?ticket=${ticket.ticket}`)).status).toBe(401)
      await expect(createRecordingTicket('0'.repeat(24), token)).rejects.toMatchObject({ status: 404, code: 'recording_not_found' })
    })

    it('deletes a recording and says so when it is gone', async () => {
      await expect(deleteRecording(target.id, token)).resolves.toMatchObject({ id: target.id, name: target.name })
      expect((await fetchRecordings(token)).recordings.map(({ id }) => id)).not.toContain(target.id)
      await expect(deleteRecording(target.id, token)).rejects.toMatchObject({ status: 404, code: 'recording_not_found' })
      await expect(deleteRecording(target.id, 'not-the-token')).rejects.toBeInstanceOf(ApiError)
    })
  })
})
