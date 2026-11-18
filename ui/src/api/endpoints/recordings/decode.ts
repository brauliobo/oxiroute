import { arrayOf, decimalString, isRecord, nullableSafeInteger, nullableString, safeInteger } from '../../../valueGuards'
import { invalidPayload } from '../shared'
import type { RecorderSource, Recording, RecordingPage, RecordingRoot } from './types'

const STATUSES = ['recording', 'complete', 'partial']
const RETENTIONS = ['reject', 'delete_oldest']

const isSource = (value: unknown): value is RecorderSource =>
  isRecord(value) && typeof value.service === 'string' && typeof value.application === 'string' &&
  typeof value.recorder === 'string'

function isRecording(value: unknown): value is Recording {
  return isRecord(value) && typeof value.id === 'string' && typeof value.name === 'string' &&
    typeof value.status === 'string' && STATUSES.includes(value.status) && nullableString(value.stream) &&
    nullableSafeInteger(value.started_at_unix_ms) && nullableSafeInteger(value.ended_at_unix_ms) &&
    nullableSafeInteger(value.duration_ms) && decimalString(value.size_bytes) &&
    safeInteger(value.modified_at_unix_ms) && nullableString(value.video_codec) &&
    nullableString(value.audio_codec) && arrayOf(value.recorders, isSource)
}

function isRoot(value: unknown): value is RecordingRoot {
  return isRecord(value) && arrayOf(value.recorders, isSource) && decimalString(value.bytes_used) &&
    (value.max_bytes === null || decimalString(value.max_bytes)) && safeInteger(value.files) &&
    nullableSafeInteger(value.max_files) && typeof value.retention === 'string' &&
    RETENTIONS.includes(value.retention) && nullableSafeInteger(value.retention_max_age_ms) &&
    decimalString(value.evicted_files) && decimalString(value.evicted_bytes)
}

export function parseRecordingPage(value: unknown): RecordingPage {
  if (!isRecord(value) || !arrayOf(value.recordings, isRecording) || !nullableString(value.next_cursor) ||
    !arrayOf(value.roots, isRoot)
  ) return invalidPayload('recording inventory')
  return value as unknown as RecordingPage
}

export function parseDeletedRecording(value: unknown): Recording {
  if (!isRecord(value) || !isRecording(value.deleted)) return invalidPayload('recording delete')
  return value.deleted
}
