export interface RtmpCapabilities {
  live_ingest: boolean
  manual_recording: boolean
}

export interface TrackSnapshot {
  codec_id: number | null
  codec_fourcc: string | null
  codec_name: string | null
  recording_supported: boolean
  payload_bytes: string
  last_rtmp_timestamp_ms: number | null
  last_observed_at_unix_ms: number | null
}

export type RecorderErrorCode =
  | 'open_failed'
  | 'quota_files'
  | 'quota_bytes'
  | 'write_failed'
  | 'close_failed'
  | 'backend_unavailable'
  | 'file_sync_failed'
  | 'publish_failed'
  | 'directory_sync_failed'
  | 'queue_discontinuity'
  | 'unsupported_codec'
  | 'shutdown_timed_out'
  | 'worker_panicked'
  | 'stale_publisher'
export type RecorderNotification = 'started' | 'stopped' | 'failed'
export type RecorderPhase =
  | { state: 'idle' }
  | { state: 'starting'; operation_id: string }
  | { state: 'recording'; operation_id: string; started_at_unix_ms: number }
  | { state: 'stopping'; operation_id: string }
  | { state: 'failed'; operation_id: string; code: RecorderErrorCode }

export interface RecorderSnapshot {
  id: string
  name: string | null
  manual: boolean
  phase: RecorderPhase
  changed_at_unix_ms: number
  bytes_written: string
  current_relative_name: string | null
  published_but_not_durable_relative_name: string | null
  segments_started: string
  segments_completed: string
  discontinuities: string
  last_completed_relative_name: string | null
  recoverable_partial_name: string | null
  last_notification: RecorderNotification | null
}

export type RtmpRelayPhase = 'connecting' | 'publishing' | 'pulling' | 'backoff' | 'stopped'
export type RtmpRelayFailure = 'policy' | 'connect' | 'handshake' | 'session' | 'transport' | 'source' | 'thread'
export type RtmpRelayDnsRefreshFailure = 'resolution' | 'address_set' | 'policy' | 'direct_loop' | 'family_mismatch'

export interface RelaySnapshot {
  id: string
  destination: { address: string; application: string; stream_name: string }
  phase: RtmpRelayPhase
  last_failure: RtmpRelayFailure | null
  queue_messages: number
  queue_bytes: string
  connection_attempts: string
  connections: string
  reconnects: string
  dns_refresh_attempts: string
  dns_refresh_successes: string
  dns_refresh_failures: string
  last_dns_refresh_failure: RtmpRelayDnsRefreshFailure | null
  events_enqueued: string
  events_sent: string
  events_dropped: string
  payload_bytes_sent: string
}

export type RtmpStreamState = 'live' | 'idle'
export type RtmpEndReason = 'closed' | 'dropped' | 'stale' | 'shutdown' | 'failed'

export interface StreamProfile {
  video_width: number | null
  video_height: number | null
  frame_rate_millihertz: number | null
  video_bitrate_bps: number | null
  audio_bitrate_bps: number | null
  keyframe_interval_ms: number | null
  audio_sample_rate_hz: number | null
  audio_channels: number | null
}

export interface StreamMedia {
  audio: TrackSnapshot
  video: TrackSnapshot
  profile: StreamProfile
  fanout_payload_bytes: string
}

export interface StreamSnapshot {
  id: string
  revision: string
  server_id: string
  application: string
  name: string
  state: RtmpStreamState
  created_at_unix_ms: number
  publisher: {
    session_id: string
    attached_at_unix_ms: number
  } | null
  subscriber_count: number
  media: StreamMedia
  relays: RelaySnapshot[]
  recording_supported: boolean
  manual_recording: boolean
  recorders: RecorderSnapshot[]
}

export interface EndedStreamSnapshot {
  id: string
  server_id: string
  application: string
  name: string
  state: 'offline'
  created_at_unix_ms: number
  published_at_unix_ms: number
  last_seen_at_unix_ms: number
  ended_at_unix_ms: number
  end_reason: RtmpEndReason
  publisher_session_id: string
  media: StreamMedia
}

export interface RtmpCatalog {
  revision: string
  as_of_unix_ms: number
  capabilities: RtmpCapabilities
  streams: StreamSnapshot[]
  recently_ended_streams: EndedStreamSnapshot[]
}

export type RtmpClientRole = 'client' | 'publisher' | 'subscriber'
export type RtmpClientControlTarget = RtmpClientRole

export interface RtmpClientSnapshot {
  id: string
  service: string
  peerIp: string | null
  connectedAtUnixMs: number
  application: string | null
  stream: string | null
  role: RtmpClientRole
  revision: string
}

export interface RtmpStatsGlobal {
  activeStreams: number
  publishers: number
  subscribers: number
  audioPayloadBytes: string
  videoPayloadBytes: string
  liveIngest: boolean
  manualRecording: boolean
}

export interface RtmpLiveStat {
  id: string
  service: string
  application: string
  name: string
  createdAtUnixMs: number
  publisherSessionId: string | null
  subscriberCount: number
  audioPayloadBytes: string
  videoPayloadBytes: string
}

export interface RtmpStats {
  revision: string
  asOfUnixMs: number
  global: RtmpStatsGlobal
  live: RtmpLiveStat[]
  clients: RtmpClientSnapshot[]
  liveTruncated: boolean
  clientsTruncated: boolean
}

export interface RtmpControlResponse {
  outcome: 'requested' | 'already_requested'
  sessionId: string
  target: RtmpClientControlTarget
  sessionRevision: string
}
