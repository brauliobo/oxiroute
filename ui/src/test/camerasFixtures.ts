import type {
  EndedStreamSnapshot,
  RecorderSnapshot,
  Recording,
  RecordingPage,
  RecordingRoot,
  RtmpCatalog,
  RtmpClientSnapshot,
  RtmpStats,
  SecretFingerprint,
  StreamProfile,
  StreamSnapshot,
  TrackSnapshot,
} from '../api'

export const SERVICE = 'edge'
const SECOND = 1000

export const sessionIds = { front: 'sess-front-pub', garage: 'sess-garage-pub', side: 'sess-side-pub' } as const

const profile = (over: Partial<StreamProfile> = {}): StreamProfile => ({
  video_width: 1920,
  video_height: 1080,
  frame_rate_millihertz: 30_000,
  video_bitrate_bps: 4_000_000,
  audio_bitrate_bps: 128_000,
  keyframe_interval_ms: 2_000,
  audio_sample_rate_hz: 48_000,
  audio_channels: 2,
  ...over,
})

const track = (name: 'avc' | 'aac' | 'hevc', observedAt: number | null): TrackSnapshot => ({
  codec_id: name === 'avc' ? 7 : name === 'aac' ? 10 : null,
  codec_fourcc: name === 'hevc' ? 'hvc1' : null,
  codec_name: name,
  recording_supported: true,
  payload_bytes: '1048576',
  last_rtmp_timestamp_ms: 1000,
  last_observed_at_unix_ms: observedAt,
})

const idleRecorder = (id: string, name: string, manual: boolean, now: number): RecorderSnapshot => ({
  id,
  name,
  manual,
  phase: { state: 'idle' },
  changed_at_unix_ms: now - 60 * SECOND,
  bytes_written: '0',
  current_relative_name: null,
  published_but_not_durable_relative_name: null,
  segments_started: '0',
  segments_completed: '0',
  discontinuities: '0',
  last_completed_relative_name: null,
  recoverable_partial_name: null,
  last_notification: null,
})

const recordingRecorder = (id: string, name: string, now: number): RecorderSnapshot => ({
  ...idleRecorder(id, name, false, now),
  phase: { state: 'recording', operation_id: `op-${id}`, started_at_unix_ms: now - 600 * SECOND },
  bytes_written: '5242880',
  current_relative_name: 'cam-front-2026-01-01T10-00-00.flv',
  segments_started: '2',
  last_notification: 'started',
})

function stream(
  key: 'front' | 'garage' | 'side' | 'spare',
  now: number,
  over: Partial<StreamSnapshot> & { observedAt?: number | null; profile?: Partial<StreamProfile> },
): StreamSnapshot {
  const { observedAt = now - 500, profile: profileOver, recorders = [], ...rest } = over
  return {
    id: `stream-${key}`,
    revision: '3',
    server_id: SERVICE,
    application: 'live',
    name: `cam-${key}`,
    state: 'live',
    created_at_unix_ms: now - 3_600 * SECOND,
    publisher: { session_id: `sess-${key}-pub`, attached_at_unix_ms: now - 3_600 * SECOND },
    subscriber_count: 0,
    media: {
      audio: track('aac', observedAt),
      video: track('avc', observedAt),
      profile: profile(profileOver),
      fanout_payload_bytes: '0',
    },
    relays: [],
    recording_supported: recorders.length > 0,
    manual_recording: recorders.some(({ manual }) => manual),
    recorders,
    ...rest,
  }
}

function ended(key: 'yard' | 'lobby', now: number, over: Partial<EndedStreamSnapshot>): EndedStreamSnapshot {
  return {
    id: `stream-${key}`,
    server_id: SERVICE,
    application: 'live',
    name: `cam-${key}`,
    state: 'offline',
    created_at_unix_ms: now - 7_200 * SECOND,
    published_at_unix_ms: now - 7_200 * SECOND,
    last_seen_at_unix_ms: now - 300 * SECOND,
    ended_at_unix_ms: now - 295 * SECOND,
    end_reason: 'stale',
    publisher_session_id: `sess-${key}-pub`,
    media: { audio: track('aac', now - 300 * SECOND), video: track('avc', now - 300 * SECOND), profile: profile({ video_width: 1280, video_height: 720, video_bitrate_bps: 2_000_000 }), fanout_payload_bytes: '0' },
    ...over,
  }
}

export function camerasCatalog(now = Date.now()): RtmpCatalog {
  const front = stream('front', now, {
    subscriber_count: 3,
    recorders: [idleRecorder('rec-archive', 'archive', true, now), recordingRecorder('rec-continuous', 'continuous', now)],
  })
  const garage = stream('garage', now, {
    profile: { video_width: 1280, video_height: 720, frame_rate_millihertz: 25_000, video_bitrate_bps: 1_500_000 },
    recorders: [{
      ...idleRecorder('rec-garage', 'continuous', false, now),
      phase: { state: 'failed', operation_id: 'op-garage', code: 'quota_files' },
      last_notification: 'failed',
    }],
  })
  const side = stream('side', now, { observedAt: now - 40 * SECOND, profile: { video_bitrate_bps: null, frame_rate_millihertz: null } })
  const spare = stream('spare', now, { state: 'idle', publisher: null, subscriber_count: 1 })
  return {
    revision: '9',
    as_of_unix_ms: now,
    capabilities: { live_ingest: true, manual_recording: true },
    streams: [front, garage, side, spare],
    recently_ended_streams: [
      ended('yard', now, {}),
      ended('lobby', now, { end_reason: 'dropped', ended_at_unix_ms: now - 900 * SECOND, last_seen_at_unix_ms: now - 905 * SECOND }),
    ],
  }
}

const client = (id: string, role: 'publisher' | 'subscriber', stream: string, peerIp: string, now: number): RtmpClientSnapshot => ({
  id,
  service: SERVICE,
  peerIp,
  connectedAtUnixMs: now - 1_800 * SECOND,
  application: 'live',
  stream,
  role,
  revision: '11',
})

export function camerasStats(now = Date.now()): RtmpStats {
  const clients = [
    client(sessionIds.front, 'publisher', 'cam-front', '192.0.2.10', now),
    client(sessionIds.garage, 'publisher', 'cam-garage', '192.0.2.11', now),
    client(sessionIds.side, 'publisher', 'cam-side', '192.0.2.12', now),
    client('sess-view-1', 'subscriber', 'cam-front', '198.51.100.21', now),
    client('sess-view-2', 'subscriber', 'cam-front', '198.51.100.22', now),
    client('sess-view-3', 'subscriber', 'cam-front', '198.51.100.23', now),
  ]
  return {
    revision: '11',
    asOfUnixMs: now,
    global: {
      activeStreams: 4, publishers: 3, subscribers: 4, audioPayloadBytes: '0', videoPayloadBytes: '0',
      liveIngest: true, manualRecording: true,
    },
    live: [],
    clients,
    liveTruncated: false,
    clientsTruncated: false,
  }
}

const recorders = (application: string) => [{ service: SERVICE, application, recorder: 'archive' }]

export const GIB = 1024 ** 3

export function recording(index: number, over: Partial<Recording> = {}): Recording {
  const start = Date.UTC(2026, 0, 1, 10, 0, 0) + index * 3_600_000
  const name = `cam-front-2026-01-01T${String(10 + index).padStart(2, '0')}-00-00.flv`
  return {
    id: `0123456789abcdef0123${String(index).padStart(4, '0')}`,
    name,
    status: 'complete',
    stream: 'cam-front',
    started_at_unix_ms: start,
    ended_at_unix_ms: start + 1_800_000,
    duration_ms: 1_800_000,
    size_bytes: String(450 * 1024 * 1024 + index * 1024),
    modified_at_unix_ms: start + 1_800_000,
    video_codec: 'avc',
    audio_codec: 'aac',
    recorders: recorders('live'),
    ...over,
  }
}

export const recordingRoots = (): RecordingRoot[] => [{
  recorders: recorders('live'),
  bytes_used: String(7 * GIB),
  max_bytes: String(10 * GIB),
  files: 4,
  max_files: 10,
  retention: 'delete_oldest',
  retention_max_age_ms: 7 * 86_400_000,
  evicted_files: '3',
  evicted_bytes: String(Math.round(1.5 * GIB)),
}, {
  recorders: [{ service: SERVICE, application: 'archive', recorder: 'cold' }],
  bytes_used: String(Math.round(9.7 * GIB)),
  max_bytes: String(10 * GIB),
  files: 10,
  max_files: 10,
  retention: 'reject',
  retention_max_age_ms: null,
  evicted_files: '0',
  evicted_bytes: '0',
}]

export function recordingPage(over: Partial<RecordingPage> = {}): RecordingPage {
  return {
    recordings: [
      recording(3, { status: 'recording', ended_at_unix_ms: null, duration_ms: null }),
      recording(2),
      recording(1, { stream: 'cam-garage', video_codec: 'hevc', size_bytes: String(Math.round(1.2 * GIB)) }),
      recording(0, { status: 'partial', duration_ms: null, ended_at_unix_ms: null, audio_codec: null }),
    ],
    next_cursor: null,
    roots: recordingRoots(),
    ...over,
  }
}

export const secretFingerprints = (): SecretFingerprint[] => [
  { service: SERVICE, application: 'archive', role: 'publish', fingerprint: 'fp-0a1b2c3d' },
  { service: SERVICE, application: 'live', role: 'play', fingerprint: 'fp-5e6f7a8b' },
  { service: SERVICE, application: 'live', role: 'publish', fingerprint: 'fp-1a2b3c4d' },
]
