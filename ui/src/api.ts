export interface RtmpCapabilities {
  live_ingest: boolean
  manual_recording: boolean
}

export interface TrackSnapshot {
  codec_id: number | null
  codec_name: string | null
  payload_bytes: string
  last_rtmp_timestamp_ms: number | null
  last_observed_at_unix_ms: number | null
}

export interface RecorderPhase {
  state: 'idle' | 'starting' | 'recording' | 'stopping' | 'failed'
  operation_id?: string
  started_at_unix_ms?: number
  code?: string
}

export interface RecorderSnapshot {
  id: string
  name: string | null
  manual: boolean
  phase: RecorderPhase
  changed_at_unix_ms: number
  bytes_written: string
}

export interface StreamSnapshot {
  id: string
  revision: string
  server_id: string
  application: string
  name: string
  created_at_unix_ms: number
  publisher: {
    session_id: string
    attached_at_unix_ms: number
  } | null
  subscriber_count: number
  media: {
    audio: TrackSnapshot
    video: TrackSnapshot
    fanout_payload_bytes: string
  }
  recorders: RecorderSnapshot[]
}

export interface RtmpCatalog {
  revision: string
  as_of_unix_ms: number
  capabilities: RtmpCapabilities
  streams: StreamSnapshot[]
}

interface ErrorResponse {
  error?: {
    code?: string
    message?: string
  }
}

export async function fetchRtmpCatalog(signal?: AbortSignal): Promise<RtmpCatalog> {
  return request<RtmpCatalog>('/api/v1/rtmp/streams', { signal })
}

export async function setRecording(
  streamId: string,
  recorderId: string,
  action: 'start' | 'stop',
): Promise<RecorderSnapshot> {
  return request<RecorderSnapshot>(
    `/api/v1/rtmp/streams/${streamId}/recorders/${recorderId}/${action}`,
    { method: 'POST' },
  )
}

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init)
  const payload = (await response.json()) as T & ErrorResponse
  if (!response.ok) {
    throw new Error(payload.error?.message ?? `Request failed with status ${response.status}`)
  }
  return payload
}
