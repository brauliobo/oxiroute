export type RecordingStatus = 'recording' | 'complete' | 'partial'
export type RecordingRetention = 'reject' | 'delete_oldest'

export interface RecorderSource {
  service: string
  application: string
  recorder: string
}

export interface Recording {
  id: string
  name: string
  status: RecordingStatus
  stream: string | null
  started_at_unix_ms: number | null
  ended_at_unix_ms: number | null
  duration_ms: number | null
  size_bytes: string
  modified_at_unix_ms: number
  video_codec: string | null
  audio_codec: string | null
  recorders: RecorderSource[]
}

export interface RecordingRoot {
  recorders: RecorderSource[]
  bytes_used: string
  max_bytes: string | null
  files: number
  max_files: number | null
  retention: RecordingRetention
  retention_max_age_ms: number | null
  evicted_files: string
  evicted_bytes: string
}

export interface RecordingPage {
  recordings: Recording[]
  next_cursor: string | null
  roots: RecordingRoot[]
}

export interface RecordingFilter {
  stream?: string
  recorder?: string
  status?: RecordingStatus
  limit?: number
  cursor?: string
}
