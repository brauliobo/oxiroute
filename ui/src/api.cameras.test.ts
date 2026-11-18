import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  ApiError,
  createMediaTicket,
  createRecordingTicket,
  deleteRecording,
  fetchRecordings,
  fetchRtmpCatalog,
  fetchSecretFingerprints,
} from './api'
import { camerasCatalog, recording, recordingPage, secretFingerprints } from './test/camerasFixtures'

afterEach(() => vi.unstubAllGlobals())

function stubFetch(response: Response): ReturnType<typeof vi.fn> {
  const fetch = vi.fn(() => Promise.resolve(response))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

const sent = (fetch: ReturnType<typeof vi.fn>) => fetch.mock.calls[0] as unknown as [string, RequestInit]

describe('stream catalog', () => {
  it('keeps the stream state, the measured profile and the recently ended streams', async () => {
    stubFetch(Response.json(camerasCatalog(1_750_000_000_000)))
    const catalog = await fetchRtmpCatalog('token')
    expect(catalog.streams.map(({ name, state }) => [name, state])).toEqual([
      ['cam-front', 'live'], ['cam-garage', 'live'], ['cam-side', 'live'], ['cam-spare', 'idle'],
    ])
    expect(catalog.streams[0]!.media.profile).toMatchObject({ video_width: 1920, video_bitrate_bps: 4_000_000 })
    expect(catalog.streams[2]!.media.profile.video_bitrate_bps).toBeNull()
    expect(catalog.recently_ended_streams.map(({ name, end_reason }) => [name, end_reason])).toEqual([
      ['cam-yard', 'stale'], ['cam-lobby', 'dropped'],
    ])
  })

  it('accepts a recorder that failed on a file or byte quota', async () => {
    const payload = camerasCatalog()
    stubFetch(Response.json(payload))
    const catalog = await fetchRtmpCatalog('token')
    expect(catalog.streams[1]!.recorders[0]!.phase).toEqual({ state: 'failed', operation_id: 'op-garage', code: 'quota_files' })
    const bytes = camerasCatalog()
    bytes.streams[1]!.recorders[0]!.phase = { state: 'failed', operation_id: 'op-garage', code: 'quota_bytes' }
    stubFetch(Response.json(bytes))
    await expect(fetchRtmpCatalog('token')).resolves.toBeDefined()
  })

  it('rejects a catalog without a profile, a state or the ended list', async () => {
    const noProfile = camerasCatalog() as unknown as { streams: { media: Record<string, unknown> }[] }
    delete noProfile.streams[0]!.media.profile
    stubFetch(Response.json(noProfile))
    await expect(fetchRtmpCatalog('token')).rejects.toThrow('invalid response payload')

    const unknownState = camerasCatalog()
    ;(unknownState.streams[0] as unknown as { state: string }).state = 'paused'
    stubFetch(Response.json(unknownState))
    await expect(fetchRtmpCatalog('token')).rejects.toThrow('invalid response payload')

    const noEnded: Record<string, unknown> = { ...camerasCatalog() }
    delete noEnded.recently_ended_streams
    stubFetch(Response.json(noEnded))
    await expect(fetchRtmpCatalog('token')).rejects.toThrow('invalid response payload')

    const unknownReason = camerasCatalog()
    ;(unknownReason.recently_ended_streams[0] as unknown as { end_reason: string }).end_reason = 'vanished'
    stubFetch(Response.json(unknownReason))
    await expect(fetchRtmpCatalog('token')).rejects.toThrow('invalid response payload')
  })
})

describe('recordings', () => {
  it('lists with only the filters that are set, newest first as the server sorts', async () => {
    const fetch = stubFetch(Response.json(recordingPage()))
    const page = await fetchRecordings('token', { stream: 'cam-front', status: 'complete', limit: 50, cursor: '' })
    expect(sent(fetch)[0]).toBe('/api/v1/rtmp/recordings?stream=cam-front&status=complete&limit=50')
    expect(sent(fetch)[1].headers).toEqual({ Authorization: 'Bearer token' })
    expect(page.recordings).toHaveLength(4)
    expect(page.roots[0]).toMatchObject({ retention: 'delete_oldest', retention_max_age_ms: 604_800_000 })
  })

  it('asks for the bare inventory without a query string', async () => {
    const fetch = stubFetch(Response.json(recordingPage()))
    await fetchRecordings('token')
    expect(sent(fetch)[0]).toBe('/api/v1/rtmp/recordings')
  })

  it('rejects an inventory with an unknown status or a numeric size', async () => {
    const page = recordingPage()
    ;(page.recordings[0] as unknown as { status: string }).status = 'lost'
    stubFetch(Response.json(page))
    await expect(fetchRecordings('token')).rejects.toThrow('invalid response payload')

    const numeric = recordingPage()
    ;(numeric.recordings[0] as unknown as { size_bytes: number }).size_bytes = 12
    stubFetch(Response.json(numeric))
    await expect(fetchRecordings('token')).rejects.toThrow('invalid response payload')

    const unknownRetention = recordingPage()
    ;(unknownRetention.roots[0] as unknown as { retention: string }).retention = 'forever'
    stubFetch(Response.json(unknownRetention))
    await expect(fetchRecordings('token')).rejects.toThrow('invalid response payload')
  })

  it('deletes one recording by its id and returns what was removed', async () => {
    const target = recording(2)
    const fetch = stubFetch(Response.json({ deleted: target }))
    await expect(deleteRecording(target.id, 'token')).resolves.toEqual(target)
    expect(sent(fetch)[0]).toBe(`/api/v1/rtmp/recordings/${target.id}`)
    expect(sent(fetch)[1].method).toBe('DELETE')
  })

  it('surfaces a refused delete of a recording being written', async () => {
    stubFetch(Response.json({ error: { code: 'recording_active', message: 'The recording is being written.' } }, { status: 409 }))
    await expect(deleteRecording('abc', 'token')).rejects.toMatchObject({ status: 409, code: 'recording_active' })
  })
})

describe('media tickets', () => {
  const ticket = {
    ticket: '1750000090000.c2lnbmF0dXJl',
    path: '/api/v1/rtmp/recordings/abc/content?ticket=1750000090000.c2lnbmF0dXJl',
    expiresAtUnixMs: 1_750_000_090_000,
    ttlSeconds: 90,
  }

  it('mints a recording ticket with a bearer header and an empty body', async () => {
    const fetch = stubFetch(Response.json(ticket))
    await expect(createRecordingTicket('abc', 'token')).resolves.toEqual(ticket)
    const [url, init] = sent(fetch)
    expect(url).toBe('/api/v1/rtmp/recordings/abc/ticket')
    expect(init.method).toBe('POST')
    expect(init.headers).toMatchObject({ Authorization: 'Bearer token' })
    expect(init.body).toBe('{}')
  })

  it('mints a live media ticket for the longest lifetime and encodes each path part', async () => {
    const live = { ...ticket, path: '/api/v1/rtmp/media/edge/live/cam%20front/index.m3u8?ticket=1.x', ttlSeconds: 600 }
    const fetch = stubFetch(Response.json(live))
    await createMediaTicket('edge', 'live', 'cam front', 'token')
    expect(sent(fetch)[0]).toBe('/api/v1/rtmp/media/edge/live/cam%20front/ticket')
    expect(sent(fetch)[1].body).toBe('{"ttlSeconds":600}')
  })

  it('refuses a ticket path that leaves the RTMP API or lacks the ticket', async () => {
    stubFetch(Response.json({ ...ticket, path: 'https://example.com/x?ticket=1.x' }))
    await expect(createRecordingTicket('abc', 'token')).rejects.toThrow('invalid response payload')
    stubFetch(Response.json({ ...ticket, path: '/api/v1/rtmp/recordings/abc/content' }))
    await expect(createRecordingTicket('abc', 'token')).rejects.toThrow('invalid response payload')
  })

  it('never puts the ticket into an error message', async () => {
    stubFetch(Response.json({ ...ticket, path: '/elsewhere?ticket=1.secret-part' }))
    const failure = await createRecordingTicket('abc', 'token').catch((error: unknown) => error)
    expect(String((failure as Error).message)).not.toContain('secret-part')
  })

  it('reports a missing media output as an API error', async () => {
    stubFetch(Response.json({ error: { code: 'media_not_found', message: 'no output' } }, { status: 404 }))
    await expect(createMediaTicket('edge', 'live', 'cam', 'token')).rejects.toBeInstanceOf(ApiError)
  })
})

describe('secret fingerprints', () => {
  it('lists the fingerprints and nothing else', async () => {
    const fetch = stubFetch(Response.json({ secrets: [{ ...secretFingerprints()[0]!, secret: 'must-not-survive' }] }))
    const list = await fetchSecretFingerprints('token')
    expect(sent(fetch)[0]).toBe('/api/v1/rtmp/secrets')
    expect(list).toEqual([secretFingerprints()[0]])
    expect(JSON.stringify(list)).not.toContain('must-not-survive')
  })

  it('rejects anything that is not a short fingerprint', async () => {
    stubFetch(Response.json({ secrets: [{ ...secretFingerprints()[0]!, fingerprint: 'hunter2' }] }))
    await expect(fetchSecretFingerprints('token')).rejects.toThrow('invalid response payload')
    stubFetch(Response.json({ secrets: [{ ...secretFingerprints()[0]!, role: 'admin' }] }))
    await expect(fetchSecretFingerprints('token')).rejects.toThrow('invalid response payload')
  })
})
