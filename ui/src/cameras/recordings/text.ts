import { formatBytes, formatDuration, formatTimestamp, UNKNOWN } from '#design/format'
import type { Recording } from '../../api'

export const startedText = (recording: Recording): string =>
  recording.started_at_unix_ms === null
    ? `${formatTimestamp(recording.modified_at_unix_ms)} (modified)`
    : formatTimestamp(recording.started_at_unix_ms)

export const durationText = (recording: Recording): string =>
  recording.duration_ms === null ? UNKNOWN : formatDuration(recording.duration_ms)

export const sizeText = (recording: Recording): string => formatBytes(recording.size_bytes)

export const codecText = (recording: Recording): string =>
  `${recording.video_codec ?? 'no video'} / ${recording.audio_codec ?? 'no audio'}`
