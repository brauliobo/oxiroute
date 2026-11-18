import { describe, expect, it } from 'vitest'
import { camerasCatalog, camerasStats } from '../../test/camerasFixtures'
import { cameraFacts } from './facts'
import { NO_FILTER, filterCameras, summarize } from './filter'
import { STALL_AFTER_MS, buildCameras } from './model'

const NOW = 1_750_000_000_000
const build = (now = NOW) => buildCameras(camerasCatalog(now), camerasStats(now), now)
const byName = (name: string) => build().find((camera) => camera.name === name)!

describe('camera model', () => {
  it('lists problems first, then live, idle and the streams that ended, newest ending first', () => {
    expect(build().map(({ name, status }) => [name, status])).toEqual([
      ['cam-garage', 'degraded'],
      ['cam-side', 'degraded'],
      ['cam-front', 'streaming'],
      ['cam-spare', 'idle'],
      ['cam-yard', 'offline'],
      ['cam-lobby', 'offline'],
    ])
  })

  it('describes a healthy live camera by what the publisher reports', () => {
    const front = byName('cam-front')
    expect(front).toMatchObject({
      title: 'live / cam-front',
      service: 'edge',
      video: 'AVC',
      audio: 'AAC',
      resolution: '1920x1080',
      frameRate: '30 fps',
      bitrate: '4.1 Mbit/s',
      viewers: 3,
      uptimeMs: 3_600_000,
      reasons: [],
    })
    expect(front.publisher).toMatchObject({ sessionId: 'sess-front-pub', peer: '192.0.2.10' })
    expect(front.viewerClients.map(({ peerIp }) => peerIp)).toEqual(['198.51.100.21', '198.51.100.22', '198.51.100.23'])
  })

  it('formats unknown measurements as absent instead of zero', () => {
    const side = byName('cam-side')
    expect(side.frameRate).toBeNull()
    expect(side.bitrate).toBe('128 kbit/s')
    const bare = camerasCatalog(NOW)
    const profile = bare.streams[2]!.media.profile
    Object.assign(profile, { video_bitrate_bps: null, audio_bitrate_bps: null, video_width: null })
    expect(buildCameras(bare, null, NOW).find(({ name }) => name === 'cam-side')).toMatchObject({
      bitrate: null, resolution: null,
    })
  })

  it('calls a stream degraded when a recorder failed and says why in words', () => {
    const garage = byName('cam-garage')
    expect(garage.status).toBe('degraded')
    expect(garage.reasons).toEqual([
      'Recorder continuous failed: the folder reached its file limit and its retention does not free space (quota_files).',
    ])
    expect(garage.recorders[0]).toMatchObject({ label: 'FAILED', tone: 'bad', recording: false, action: null })
  })

  it('calls a stream degraded when its media stopped for longer than the stall limit', () => {
    const side = byName('cam-side')
    expect(side.status).toBe('degraded')
    expect(side.reasons).toEqual(['No media for 40 s; the publisher may be stalled.'])
    const fresh = camerasCatalog(NOW)
    fresh.streams[2]!.media.video.last_observed_at_unix_ms = NOW - STALL_AFTER_MS
    fresh.streams[2]!.media.audio.last_observed_at_unix_ms = NOW - STALL_AFTER_MS
    expect(buildCameras(fresh, null, NOW).find(({ name }) => name === 'cam-side')!.status).toBe('streaming')
  })

  it('does not call a stream stalled before any media was observed', () => {
    const catalog = camerasCatalog(NOW)
    catalog.streams[0]!.media.video.last_observed_at_unix_ms = null
    catalog.streams[0]!.media.audio.last_observed_at_unix_ms = null
    expect(buildCameras(catalog, null, NOW).find(({ name }) => name === 'cam-front')!.status).toBe('streaming')
  })

  it('offers start on an idle manual recorder and stop on one that records, and nothing on an automatic one', () => {
    const [archive, continuous] = byName('cam-front').recorders
    expect(archive).toMatchObject({ name: 'archive', manual: true, action: 'start', label: 'IDLE' })
    expect(continuous).toMatchObject({ name: 'continuous', manual: false, action: null, label: 'RECORDING', recording: true })
    expect(continuous!.file).toBe('cam-front-2026-01-01T10-00-00.flv')
  })

  it('offers no recorder command without manual recording support', () => {
    const catalog = camerasCatalog(NOW)
    catalog.capabilities.manual_recording = false
    expect(buildCameras(catalog, null, NOW).find(({ name }) => name === 'cam-front')!.recorders.map(({ action }) => action))
      .toEqual([null, null])
  })

  it('keeps an idle stream without a publisher and without uptime', () => {
    expect(byName('cam-spare')).toMatchObject({ status: 'idle', publisher: null, uptimeMs: null, viewers: 1 })
  })

  it('shows an offline camera from the catalog with when and why it ended', () => {
    const yard = byName('cam-yard')
    expect(yard).toMatchObject({
      status: 'offline', lastSeenAt: NOW - 300_000, endedAt: NOW - 295_000, resolution: '1280x720', bitrate: '2.1 Mbit/s',
      endReason: 'The publisher stopped sending media.', publisher: null, viewers: 0,
    })
    expect(byName('cam-lobby').endReason).toBe('An operator dropped the publisher.')
  })

  it('shows a stream that publishes again once, as live', () => {
    const catalog = camerasCatalog(NOW)
    catalog.recently_ended_streams.push({ ...catalog.recently_ended_streams[0]!, id: 'stream-front', name: 'cam-front' })
    const names = buildCameras(catalog, null, NOW).filter(({ name }) => name === 'cam-front')
    expect(names.map(({ status }) => status)).toEqual(['streaming'])
  })

  it('has no publisher control or viewer list before the client list arrives', () => {
    const front = buildCameras(camerasCatalog(NOW), null, NOW).find(({ name }) => name === 'cam-front')!
    expect(front.publisher).toMatchObject({ peer: null, client: null })
    expect(front.viewerClients).toEqual([])
    expect(front.viewers).toBe(3)
  })
})

describe('camera facts', () => {
  it('lists the live facts in a fixed order', () => {
    expect(cameraFacts(byName('cam-front'), NOW).map(({ label, value }) => [label, value])).toEqual([
      ['Picture', '1920x1080 at 30 fps'],
      ['Codecs', 'AVC / AAC'],
      ['Bitrate', '4.1 Mbit/s'],
      ['Publisher', '192.0.2.10'],
      ['Viewers', '3 viewers'],
      ['Uptime', '1h 0m'],
    ])
  })

  it('lists what an offline camera was doing when it ended', () => {
    expect(cameraFacts(byName('cam-yard'), NOW).map(({ label, value }) => [label, value])).toEqual([
      ['Last seen', '5m 0s ago'],
      ['Ended', '2025-06-15 15:01:45 UTC'],
      ['Why', 'The publisher stopped sending media.'],
      ['Last picture', '1280x720 at 30 fps'],
      ['Codecs', 'AVC / AAC'],
      ['Bitrate', '2.1 Mbit/s'],
    ])
  })
})

describe('camera filter', () => {
  it('filters by state and by a case-insensitive part of the title', () => {
    const cameras = build()
    expect(filterCameras(cameras, NO_FILTER)).toHaveLength(6)
    expect(filterCameras(cameras, { query: '', status: 'degraded' }).map(({ name }) => name)).toEqual(['cam-garage', 'cam-side'])
    expect(filterCameras(cameras, { query: ' FRONT ', status: 'all' }).map(({ name }) => name)).toEqual(['cam-front'])
    expect(filterCameras(cameras, { query: 'front', status: 'offline' })).toEqual([])
  })

  it('counts each state and the cameras that record now', () => {
    expect(summarize(build())).toEqual({ all: 6, streaming: 1, degraded: 2, idle: 1, offline: 2, recording: 1 })
  })
})
