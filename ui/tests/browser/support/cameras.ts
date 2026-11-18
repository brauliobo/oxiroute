import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import type { Page, Request } from '@playwright/test'

import type { Recording } from '../../../src/api'
import {
  camerasCatalog,
  camerasStats,
  recording,
  recordingRoots,
  secretFingerprints,
} from '../../../src/test/camerasFixtures'
import { contractMonitoring } from '../../../src/test/contractFixtures'
import { managementGeneration } from '../../../src/test/managementFixtures'
import { installApiMock, json, requestPath } from '../support'
import type { ApiMockResponse } from '../support'

export const TICKET = '1999999999999.c2lnbmF0dXJl'
const PREVIEW_SEGMENT = readFileSync(join(import.meta.dirname, '../fixtures/preview.mpegts'))
const PLAYLIST = (segment: string) =>
  `#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1.0,\n${segment}\n#EXT-X-ENDLIST\n`

export interface Sent {
  method: string
  path: string
  query: string
  authorization: string | null
  body: unknown
}

export interface CameraMock {
  sent: Sent[]
  mutations: () => Sent[]
  recordings: Recording[]
}

export type CameraOverride = (request: Request) => ApiMockResponse | undefined

export function manyRecordings(count: number): Recording[] {
  return Array.from({ length: count }, (_, index) => recording(index, {
    id: `0123456789abcdef${String(index).padStart(8, '0')}`,
    name: `cam-front-2026-01-${String(1 + Math.floor(index / 24)).padStart(2, '0')}T${String(index % 24).padStart(2, '0')}-00-00.flv`,
    stream: index % 3 === 0 ? 'cam-garage' : 'cam-front',
    started_at_unix_ms: Date.UTC(2026, 0, 1) + index * 3_600_000,
    ended_at_unix_ms: Date.UTC(2026, 0, 1) + index * 3_600_000 + 1_800_000,
  }))
}

const ticketBody = (path: string) => json({ ticket: TICKET, path: `${path}?ticket=${TICKET}`, expiresAtUnixMs: Date.now() + 600_000, ttlSeconds: 600 })

function listRecordings(all: Recording[], params: URLSearchParams): ApiMockResponse {
  const limit = Number(params.get('limit') ?? 100)
  const stream = params.get('stream')
  const status = params.get('status')
  const matching = all.filter((entry) => (!stream || entry.stream === stream) && (!status || entry.status === status))
  const offset = Number(params.get('cursor') ?? 0)
  const next = offset + limit
  return json({
    recordings: matching.slice(offset, next),
    next_cursor: next < matching.length ? String(next) : null,
    roots: recordingRoots(),
  })
}

export async function mockCameras(page: Page, options: { recordings?: Recording[]; override?: CameraOverride } = {}): Promise<CameraMock> {
  const sent: Sent[] = []
  const recordings = options.recordings ?? manyRecordings(4)
  await installApiMock(page, (request) => {
    const url = new URL(request.url())
    const path = requestPath(request)
    const method = request.method()
    const body = request.postData() ? JSON.parse(request.postData()!) as unknown : null
    sent.push({ method, path, query: url.search, authorization: request.headers().authorization ?? null, body })
    const overridden = options.override?.(request)
    if (overridden) return overridden
    return method === 'GET' ? read(path, url, recordings) : write(path, method, recordings)
  })
  return { sent, mutations: () => sent.filter(({ method }) => method !== 'GET'), recordings }
}

function read(path: string, url: URL, recordings: Recording[]): ApiMockResponse | undefined {
  const now = Date.now()
  if (path === '/api/v1/monitoring') return json({ ...contractMonitoring(), sampledAtUnixMs: now })
  if (path === '/api/v1/rtmp/streams') return json(camerasCatalog(now))
  if (path === '/api/v1/rtmp/stats') return json(camerasStats(now))
  if (path === '/api/v1/generations') return json(managementGeneration())
  if (path === '/api/v1/rtmp/recordings') return listRecordings(recordings, url.searchParams)
  if (path === '/api/v1/rtmp/secrets') return json({ secrets: secretFingerprints() })
  if (path.endsWith('/index.m3u8') || path.endsWith('/preview.ts')) return media(path, url)
  if (path.endsWith('/content')) return content(url)
  return undefined
}

function media(path: string, url: URL): ApiMockResponse {
  if (url.searchParams.get('ticket') !== TICKET) return json({ error: { code: 'invalid_ticket', message: 'invalid ticket' } }, 401)
  if (path.endsWith('.ts')) return { kind: 'raw', contentType: 'video/mp2t', body: PREVIEW_SEGMENT }
  return {
    kind: 'raw',
    contentType: 'application/vnd.apple.mpegurl',
    body: PLAYLIST(`preview.ts?ticket=${TICKET}`),
    headers: { 'Cache-Control': 'no-store', 'Referrer-Policy': 'no-referrer' },
  }
}

function content(url: URL): ApiMockResponse {
  if (url.searchParams.get('ticket') !== TICKET) return json({ error: { code: 'invalid_ticket', message: 'invalid ticket' } }, 401)
  return {
    kind: 'raw',
    contentType: 'video/x-flv',
    body: 'FLV',
    headers: { 'Content-Disposition': 'attachment; filename="recording.flv"', 'Cache-Control': 'no-store' },
  }
}

function write(path: string, method: string, recordings: Recording[]): ApiMockResponse | undefined {
  const media = /^\/api\/v1\/rtmp\/media\/([^/]+)\/([^/]+)\/([^/]+)\/ticket$/.exec(path)
  if (media) return ticketBody(`/api/v1/rtmp/media/${media[1]}/${media[2]}/${media[3]}/index.m3u8`)
  const download = /^\/api\/v1\/rtmp\/recordings\/([^/]+)\/ticket$/.exec(path)
  if (download) return ticketBody(`/api/v1/rtmp/recordings/${download[1]}/content`)
  const drop = /^\/api\/v1\/rtmp\/clients\/([^/]+)\/(publisher|subscriber)\/drop$/.exec(path)
  if (drop) return json({ outcome: 'requested', sessionId: drop[1], target: drop[2], sessionRevision: '11' }, 202)
  const recorder = /^\/api\/v1\/rtmp\/streams\/[^/]+\/recorders\/[^/]+\/(start|stop)$/.exec(path)
  if (recorder) return json(camerasCatalog().streams[0]!.recorders[0])
  const item = /^\/api\/v1\/rtmp\/recordings\/([^/]+)$/.exec(path)
  if (item && method === 'DELETE') {
    const index = recordings.findIndex(({ id }) => id === item[1])
    if (index < 0) return json({ error: { code: 'recording_not_found', message: 'The recording does not exist.' } }, 404)
    return json({ deleted: recordings.splice(index, 1)[0] })
  }
  return undefined
}
