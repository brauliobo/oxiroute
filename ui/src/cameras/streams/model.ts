import type { Tone } from '#design/badge/tone'
import { formatBitRate, formatDuration } from '#design/format'
import type { StreamStatus } from '#design/status/status'
import type {
  EndedStreamSnapshot,
  RecorderSnapshot,
  RtmpCatalog,
  RtmpClientSnapshot,
  RtmpStats,
  StreamProfile,
  StreamSnapshot,
  TrackSnapshot,
} from '../../api'
import { recorderControlAction, recorderPhaseTone } from '../../recording'
import { END_REASONS, RECORDER_FAILURES, RECORDER_PHASES } from './words'

export const STALL_AFTER_MS = 10_000

export interface RecorderView {
  id: string
  name: string
  manual: boolean
  label: string
  tone: Tone
  recording: boolean
  failure: string | null
  file: string | null
  action: 'start' | 'stop' | null
}

export interface PublisherView {
  sessionId: string
  peer: string | null
  client: RtmpClientSnapshot | null
}

export interface Camera {
  id: string
  service: string
  application: string
  name: string
  title: string
  status: StreamStatus
  reasons: string[]
  video: string
  audio: string
  resolution: string | null
  frameRate: string | null
  bitrate: string | null
  viewers: number
  viewerClients: RtmpClientSnapshot[]
  publisher: PublisherView | null
  uptimeMs: number | null
  lastSeenAt: number | null
  endedAt: number | null
  endReason: string | null
  recorders: RecorderView[]
}

const TRACKS = ['audio', 'video'] as const

const codecName = (track: TrackSnapshot, none: string): string =>
  (track.codec_name ?? track.codec_fourcc ?? (track.codec_id === null ? none : `codec ${track.codec_id}`)).toUpperCase()

const resolution = ({ video_width: width, video_height: height }: StreamProfile) =>
  width === null || height === null ? null : `${width}x${height}`

const frameRate = ({ frame_rate_millihertz: millihertz }: StreamProfile) =>
  millihertz === null ? null : `${Number((millihertz / 1000).toFixed(2))} fps`

function bitrate({ video_bitrate_bps: video, audio_bitrate_bps: audio }: StreamProfile): string | null {
  const known = [video, audio].filter((value) => value !== null)
  return known.length === 0 ? null : formatBitRate(known.reduce((sum, value) => sum + value, 0))
}

function recorderView(stream: StreamSnapshot, recorder: RecorderSnapshot, manualRecording: boolean): RecorderView {
  const { phase } = recorder
  return {
    id: recorder.id,
    name: recorder.name ?? 'recorder',
    manual: recorder.manual,
    label: RECORDER_PHASES[phase.state],
    tone: recorderPhaseTone(phase.state),
    recording: phase.state === 'recording',
    failure: phase.state === 'failed' ? RECORDER_FAILURES[phase.code] : null,
    file: recorder.current_relative_name,
    action: recorderControlAction(manualRecording, stream, recorder),
  }
}

function stalledFor(stream: StreamSnapshot, now: number): number | null {
  const seen = TRACKS.map((name) => stream.media[name].last_observed_at_unix_ms).filter((time) => time !== null)
  if (seen.length === 0) return null
  const silence = now - Math.max(...seen)
  return silence > STALL_AFTER_MS ? silence : null
}

function degradedReasons(stream: StreamSnapshot, recorders: RecorderView[], now: number): string[] {
  const silence = stalledFor(stream, now)
  return [
    ...(silence === null ? [] : [`No media for ${formatDuration(silence)}; the publisher may be stalled.`]),
    ...recorders.filter(({ failure }) => failure).map(({ name, failure }) => `Recorder ${name} failed: ${failure}.`),
  ]
}

const viewersOf = (stream: StreamSnapshot, clients: readonly RtmpClientSnapshot[]) =>
  clients.filter(({ role, service, application, stream: name }) =>
    role === 'subscriber' && service === stream.server_id && application === stream.application && name === stream.name)

function liveCamera(stream: StreamSnapshot, catalog: RtmpCatalog, stats: RtmpStats | null, now: number): Camera {
  const recorders = stream.recorders.map((recorder) => recorderView(stream, recorder, catalog.capabilities.manual_recording))
  const reasons = stream.publisher ? degradedReasons(stream, recorders, now) : []
  const client = stream.publisher ? stats?.clients.find(({ id }) => id === stream.publisher?.session_id) ?? null : null
  const { profile } = stream.media
  return {
    id: stream.id,
    service: stream.server_id,
    application: stream.application,
    name: stream.name,
    title: `${stream.application} / ${stream.name}`,
    status: stream.publisher === null ? 'idle' : reasons.length > 0 ? 'degraded' : 'streaming',
    reasons,
    video: codecName(stream.media.video, 'No video'),
    audio: codecName(stream.media.audio, 'No audio'),
    resolution: resolution(profile),
    frameRate: frameRate(profile),
    bitrate: bitrate(profile),
    viewers: stream.subscriber_count,
    viewerClients: stats ? viewersOf(stream, stats.clients) : [],
    publisher: stream.publisher && {
      sessionId: stream.publisher.session_id,
      peer: client?.peerIp ?? null,
      client,
    },
    uptimeMs: stream.publisher && Math.max(0, now - stream.publisher.attached_at_unix_ms),
    lastSeenAt: null,
    endedAt: null,
    endReason: null,
    recorders,
  }
}

function offlineCamera(ended: EndedStreamSnapshot): Camera {
  const { profile } = ended.media
  return {
    id: ended.id,
    service: ended.server_id,
    application: ended.application,
    name: ended.name,
    title: `${ended.application} / ${ended.name}`,
    status: 'offline',
    reasons: [],
    video: codecName(ended.media.video, 'No video'),
    audio: codecName(ended.media.audio, 'No audio'),
    resolution: resolution(profile),
    frameRate: frameRate(profile),
    bitrate: bitrate(profile),
    viewers: 0,
    viewerClients: [],
    publisher: null,
    uptimeMs: null,
    lastSeenAt: ended.last_seen_at_unix_ms,
    endedAt: ended.ended_at_unix_ms,
    endReason: END_REASONS[ended.end_reason],
    recorders: [],
  }
}

const ORDER: Record<StreamStatus, number> = { degraded: 0, streaming: 1, idle: 2, offline: 3 }

export function buildCameras(catalog: RtmpCatalog, stats: RtmpStats | null, now: number): Camera[] {
  const live = catalog.streams.map((stream) => liveCamera(stream, catalog, stats, now))
  const present = new Set(live.map(({ id }) => id))
  const offline = catalog.recently_ended_streams.filter(({ id }) => !present.has(id)).map(offlineCamera)
  return [...live, ...offline].sort((a, b) =>
    ORDER[a.status] - ORDER[b.status] || (b.endedAt ?? 0) - (a.endedAt ?? 0) || a.title.localeCompare(b.title))
}
