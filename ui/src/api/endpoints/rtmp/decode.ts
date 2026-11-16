import { decimalString, isRecord, nullableString, safeInteger } from '../../../valueGuards'
import { invalidPayload } from '../shared'
import type {
  RecorderPhase,
  RecorderSnapshot,
  RelaySnapshot,
  RtmpCatalog,
  RtmpClientSnapshot,
  RtmpControlResponse,
  RtmpLiveStat,
  RtmpRelayDnsRefreshFailure,
  RtmpRelayFailure,
  RtmpRelayPhase,
  RtmpStats,
  RtmpStatsGlobal,
  StreamSnapshot,
  TrackSnapshot,
} from './types'

const RTMP_RELAY_PHASES: readonly RtmpRelayPhase[] = ['connecting', 'publishing', 'pulling', 'backoff', 'stopped']
const RTMP_RELAY_FAILURES: readonly RtmpRelayFailure[] = ['policy', 'connect', 'handshake', 'session', 'transport', 'source', 'thread']
const RTMP_RELAY_DNS_REFRESH_FAILURES: readonly RtmpRelayDnsRefreshFailure[] = [
  'resolution', 'address_set', 'policy', 'direct_loop', 'family_mismatch',
]

export function parseRtmpCatalog(value: unknown): RtmpCatalog {
  if (!isRecord(value) || !decimalString(value.revision) || !safeInteger(value.as_of_unix_ms) ||
    !isRecord(value.capabilities) || typeof value.capabilities.live_ingest !== 'boolean' ||
    typeof value.capabilities.manual_recording !== 'boolean' || !Array.isArray(value.streams) ||
    !value.streams.every(isStream)
  ) return invalidPayload('RTMP catalog')
  const manualRecording = value.capabilities.manual_recording
  if (value.streams.some((stream) =>
    stream.recording_supported !== (stream.recorders.length > 0) ||
    stream.manual_recording !== stream.recorders.some((recorder) => recorder.manual) ||
    (stream.manual_recording && !manualRecording)
  )) return invalidPayload('RTMP catalog')
  return {
    revision: value.revision,
    as_of_unix_ms: value.as_of_unix_ms,
    capabilities: {
      live_ingest: value.capabilities.live_ingest,
      manual_recording: value.capabilities.manual_recording,
    },
    streams: value.streams.map(projectStreamSnapshot),
  }
}

export function parseRtmpStats(value: unknown): RtmpStats {
  if (!isRecord(value) || !decimalString(value.revision) || !safeInteger(value.asOfUnixMs) ||
    !rtmpStatsGlobal(value.global) || !Array.isArray(value.live) || !value.live.every(rtmpLiveStat) ||
    !Array.isArray(value.clients) || !value.clients.every(rtmpClientSnapshot) ||
    typeof value.liveTruncated !== 'boolean' || typeof value.clientsTruncated !== 'boolean'
  ) return invalidPayload('RTMP statistics')
  return value as unknown as RtmpStats
}

function rtmpStatsGlobal(value: unknown): value is RtmpStatsGlobal {
  return isRecord(value) && safeInteger(value.activeStreams) && safeInteger(value.publishers) &&
    safeInteger(value.subscribers) && decimalString(value.audioPayloadBytes) &&
    decimalString(value.videoPayloadBytes) && typeof value.liveIngest === 'boolean' &&
    typeof value.manualRecording === 'boolean'
}

function rtmpLiveStat(value: unknown): value is RtmpLiveStat {
  return isRecord(value) && typeof value.id === 'string' && typeof value.service === 'string' &&
    typeof value.application === 'string' && typeof value.name === 'string' &&
    safeInteger(value.createdAtUnixMs) && nullableString(value.publisherSessionId) &&
    safeInteger(value.subscriberCount) && decimalString(value.audioPayloadBytes) &&
    decimalString(value.videoPayloadBytes)
}

function rtmpClientSnapshot(value: unknown): value is RtmpClientSnapshot {
  return isRecord(value) && typeof value.id === 'string' && typeof value.service === 'string' &&
    nullableString(value.peerIp) && safeInteger(value.connectedAtUnixMs) &&
    nullableString(value.application) && nullableString(value.stream) &&
    (value.role === 'client' || value.role === 'publisher' || value.role === 'subscriber') &&
    decimalString(value.revision)
}

export function parseRtmpControlResponse(value: unknown): RtmpControlResponse {
  if (!isRecord(value) || (value.outcome !== 'requested' && value.outcome !== 'already_requested') ||
    typeof value.sessionId !== 'string' ||
    (value.target !== 'client' && value.target !== 'publisher' && value.target !== 'subscriber') ||
    !decimalString(value.sessionRevision)
  ) return invalidPayload('RTMP control response')
  return value as unknown as RtmpControlResponse
}

function projectStreamSnapshot(stream: StreamSnapshot): StreamSnapshot {
  return {
    id: stream.id,
    revision: stream.revision,
    server_id: stream.server_id,
    application: stream.application,
    name: stream.name,
    created_at_unix_ms: stream.created_at_unix_ms,
    publisher: stream.publisher === null ? null : {
      session_id: stream.publisher.session_id,
      attached_at_unix_ms: stream.publisher.attached_at_unix_ms,
    },
    subscriber_count: stream.subscriber_count,
    media: {
      audio: projectTrackSnapshot(stream.media.audio),
      video: projectTrackSnapshot(stream.media.video),
      fanout_payload_bytes: stream.media.fanout_payload_bytes,
    },
    relays: stream.relays.map(projectRelaySnapshot),
    recording_supported: stream.recording_supported,
    manual_recording: stream.manual_recording,
    recorders: stream.recorders.map(projectRecorderSnapshot),
  }
}

function projectTrackSnapshot(track: TrackSnapshot): TrackSnapshot {
  return {
    codec_id: track.codec_id,
    codec_fourcc: track.codec_fourcc,
    codec_name: track.codec_name,
    recording_supported: track.recording_supported,
    payload_bytes: track.payload_bytes,
    last_rtmp_timestamp_ms: track.last_rtmp_timestamp_ms,
    last_observed_at_unix_ms: track.last_observed_at_unix_ms,
  }
}

function projectRelaySnapshot(relay: RelaySnapshot): RelaySnapshot {
  return {
    id: relay.id,
    destination: {
      address: relay.destination.address,
      application: relay.destination.application,
      stream_name: relay.destination.stream_name,
    },
    phase: relay.phase,
    last_failure: relay.last_failure,
    queue_messages: relay.queue_messages,
    queue_bytes: relay.queue_bytes,
    connection_attempts: relay.connection_attempts,
    connections: relay.connections,
    reconnects: relay.reconnects,
    dns_refresh_attempts: relay.dns_refresh_attempts,
    dns_refresh_successes: relay.dns_refresh_successes,
    dns_refresh_failures: relay.dns_refresh_failures,
    last_dns_refresh_failure: relay.last_dns_refresh_failure,
    events_enqueued: relay.events_enqueued,
    events_sent: relay.events_sent,
    events_dropped: relay.events_dropped,
    payload_bytes_sent: relay.payload_bytes_sent,
  }
}

function projectRecorderSnapshot(recorder: RecorderSnapshot): RecorderSnapshot {
  return {
    id: recorder.id,
    name: recorder.name,
    manual: recorder.manual,
    phase: projectRecorderPhase(recorder.phase),
    changed_at_unix_ms: recorder.changed_at_unix_ms,
    bytes_written: recorder.bytes_written,
    current_relative_name: recorder.current_relative_name,
    published_but_not_durable_relative_name: recorder.published_but_not_durable_relative_name,
    segments_started: recorder.segments_started,
    segments_completed: recorder.segments_completed,
    discontinuities: recorder.discontinuities,
    last_completed_relative_name: recorder.last_completed_relative_name,
    recoverable_partial_name: recorder.recoverable_partial_name,
    last_notification: recorder.last_notification,
  }
}

function projectRecorderPhase(phase: RecorderPhase): RecorderPhase {
  switch (phase.state) {
    case 'idle':
      return { state: phase.state }
    case 'starting':
    case 'stopping':
      return { state: phase.state, operation_id: phase.operation_id }
    case 'recording':
      return {
        state: phase.state,
        operation_id: phase.operation_id,
        started_at_unix_ms: phase.started_at_unix_ms,
      }
    case 'failed':
      return { state: phase.state, operation_id: phase.operation_id, code: phase.code }
  }
}

export function parseRecorder(value: unknown): RecorderSnapshot {
  if (!isRecorder(value)) return invalidPayload('recorder command')
  return projectRecorderSnapshot(value)
}

function isStream(value: unknown): value is StreamSnapshot {
  return isRecord(value) && typeof value.id === 'string' && decimalString(value.revision) &&
    typeof value.server_id === 'string' && typeof value.application === 'string' &&
    typeof value.name === 'string' && safeInteger(value.created_at_unix_ms) &&
    (value.publisher === null || (isRecord(value.publisher) &&
      typeof value.publisher.session_id === 'string' && safeInteger(value.publisher.attached_at_unix_ms))) &&
    safeInteger(value.subscriber_count) && isRecord(value.media) && isTrack(value.media.audio) &&
    isTrack(value.media.video) && decimalString(value.media.fanout_payload_bytes) &&
    Array.isArray(value.relays) && value.relays.every(isRelay) &&
    typeof value.recording_supported === 'boolean' && typeof value.manual_recording === 'boolean' &&
    Array.isArray(value.recorders) && value.recorders.every(isRecorder)
}

function isRelay(value: unknown): value is RelaySnapshot {
  return isRecord(value) && typeof value.id === 'string' && isRecord(value.destination) &&
    typeof value.destination.address === 'string' && typeof value.destination.application === 'string' &&
    typeof value.destination.stream_name === 'string' &&
    rtmpRelayPhase(value.phase) && (value.last_failure === null || rtmpRelayFailure(value.last_failure)) &&
    safeInteger(value.queue_messages) &&
    ['queue_bytes', 'connection_attempts', 'connections', 'reconnects', 'dns_refresh_attempts',
      'dns_refresh_successes', 'dns_refresh_failures', 'events_enqueued',
      'events_sent', 'events_dropped', 'payload_bytes_sent'].every((key) => decimalString(value[key])) &&
    (value.last_dns_refresh_failure === null || rtmpRelayDnsRefreshFailure(value.last_dns_refresh_failure))
}

function isTrack(value: unknown): value is TrackSnapshot {
  return isRecord(value) && (value.codec_id === null || safeInteger(value.codec_id)) &&
    (value.codec_fourcc === null || typeof value.codec_fourcc === 'string') &&
    (value.codec_name === null || typeof value.codec_name === 'string') &&
    typeof value.recording_supported === 'boolean' && decimalString(value.payload_bytes) &&
    (value.last_rtmp_timestamp_ms === null || safeInteger(value.last_rtmp_timestamp_ms)) &&
    (value.last_observed_at_unix_ms === null || safeInteger(value.last_observed_at_unix_ms))
}

function isRecorder(value: unknown): value is RecorderSnapshot {
  return isRecord(value) && typeof value.id === 'string' &&
    (value.name === null || typeof value.name === 'string') && typeof value.manual === 'boolean' &&
    recorderPhase(value.phase) &&
    safeInteger(value.changed_at_unix_ms) && decimalString(value.bytes_written) &&
    decimalString(value.segments_started) && decimalString(value.segments_completed) &&
    decimalString(value.discontinuities) && nullableString(value.current_relative_name) &&
    nullableString(value.published_but_not_durable_relative_name) &&
    nullableString(value.last_completed_relative_name) && nullableString(value.recoverable_partial_name) &&
    (value.last_notification === null || (typeof value.last_notification === 'string' &&
      ['started', 'stopped', 'failed'].includes(value.last_notification)))
}

function recorderPhase(value: unknown): boolean {
  if (!isRecord(value) || typeof value.state !== 'string' ||
    !['idle', 'starting', 'recording', 'stopping', 'failed'].includes(value.state)
  ) {
    return false
  }
  if (value.state === 'idle') {
    return value.operation_id === undefined && value.started_at_unix_ms === undefined && value.code === undefined
  }
  if (typeof value.operation_id !== 'string') return false
  if (value.state === 'recording') {
    return safeInteger(value.started_at_unix_ms) && value.code === undefined
  }
  if (value.state === 'failed') {
    return value.started_at_unix_ms === undefined && typeof value.code === 'string' && [
      'open_failed', 'write_failed', 'close_failed', 'backend_unavailable', 'file_sync_failed',
      'publish_failed', 'directory_sync_failed', 'queue_discontinuity', 'unsupported_codec',
      'shutdown_timed_out', 'worker_panicked', 'stale_publisher',
    ].includes(value.code)
  }
  return value.started_at_unix_ms === undefined && value.code === undefined
}

export function rtmpRelayPhase(value: unknown): value is RtmpRelayPhase {
  return typeof value === 'string' && RTMP_RELAY_PHASES.includes(value as RtmpRelayPhase)
}

export function rtmpRelayFailure(value: unknown): value is RtmpRelayFailure {
  return typeof value === 'string' && RTMP_RELAY_FAILURES.includes(value as RtmpRelayFailure)
}

export function rtmpRelayDnsRefreshFailure(value: unknown): value is RtmpRelayDnsRefreshFailure {
  return typeof value === 'string' &&
    RTMP_RELAY_DNS_REFRESH_FAILURES.includes(value as RtmpRelayDnsRefreshFailure)
}
