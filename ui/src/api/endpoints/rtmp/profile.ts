import { decimalString, isRecord, nullableSafeInteger, safeInteger } from '../../../valueGuards'
import type { EndedStreamSnapshot, RtmpEndReason, StreamMedia, StreamProfile, TrackSnapshot } from './types'

const END_REASONS: readonly RtmpEndReason[] = ['closed', 'dropped', 'stale', 'shutdown', 'failed']
const PROFILE_KEYS = [
  'video_width', 'video_height', 'frame_rate_millihertz', 'video_bitrate_bps', 'audio_bitrate_bps',
  'keyframe_interval_ms', 'audio_sample_rate_hz', 'audio_channels',
] as const satisfies readonly (keyof StreamProfile)[]
const ENDED_TEXT = ['id', 'server_id', 'application', 'name', 'publisher_session_id']
const ENDED_TIMES = ['created_at_unix_ms', 'published_at_unix_ms', 'last_seen_at_unix_ms', 'ended_at_unix_ms']

type TrackGuard = (track: unknown) => track is TrackSnapshot
type TrackProjection = (track: TrackSnapshot) => TrackSnapshot

export const isProfile = (value: unknown): value is StreamProfile =>
  isRecord(value) && PROFILE_KEYS.every((key) => nullableSafeInteger(value[key]))

const projectProfile = (profile: StreamProfile): StreamProfile =>
  Object.fromEntries(PROFILE_KEYS.map((key) => [key, profile[key]])) as unknown as StreamProfile

export const isEndedStream = (value: unknown, isTrack: TrackGuard): value is EndedStreamSnapshot =>
  isRecord(value) && value.state === 'offline' && ENDED_TEXT.every((key) => typeof value[key] === 'string') &&
  ENDED_TIMES.every((key) => safeInteger(value[key])) && END_REASONS.includes(value.end_reason as RtmpEndReason) &&
  isRecord(value.media) && isTrack(value.media.audio) && isTrack(value.media.video) && isProfile(value.media.profile) &&
  decimalString(value.media.fanout_payload_bytes)

export const projectMedia = (media: StreamMedia, projectTrack: TrackProjection): StreamMedia => ({
  audio: projectTrack(media.audio),
  video: projectTrack(media.video),
  profile: projectProfile(media.profile),
  fanout_payload_bytes: media.fanout_payload_bytes,
})

export const projectEndedStream = (stream: EndedStreamSnapshot, projectTrack: TrackProjection): EndedStreamSnapshot => ({
  id: stream.id,
  server_id: stream.server_id,
  application: stream.application,
  name: stream.name,
  state: 'offline',
  created_at_unix_ms: stream.created_at_unix_ms,
  published_at_unix_ms: stream.published_at_unix_ms,
  last_seen_at_unix_ms: stream.last_seen_at_unix_ms,
  ended_at_unix_ms: stream.ended_at_unix_ms,
  end_reason: stream.end_reason,
  publisher_session_id: stream.publisher_session_id,
  media: projectMedia(stream.media, projectTrack),
})
