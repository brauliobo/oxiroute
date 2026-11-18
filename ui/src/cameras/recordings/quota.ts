import type { Tone } from '#design/badge/tone'
import { formatBytes, formatDuration, formatExact, formatPercent } from '#design/format'
import type { RecordingRetention, RecordingRoot } from '../../api'
import { plural } from '../../workspace/plural'

export const WARN_AT = 75
export const BAD_AT = 90
export const FULL_AT = 100

export const RETENTION_WORDS: Readonly<Record<RecordingRetention, string>> = {
  reject: 'When full, new recordings are refused: the recorder stops with the failure code quota_files or quota_bytes ' +
    'and nothing is deleted.',
  delete_oldest: 'When full, the oldest finished recordings are deleted to make room (delete_oldest); ' +
    'a recording being written is never deleted.',
}

export interface Usage {
  key: 'bytes' | 'files'
  label: string
  code: 'quota_bytes' | 'quota_files'
  used: number
  max: number
  limited: boolean
  percent: number
  tone: Tone
  text: string
}

export interface QuotaAlert {
  name: string
  tone: 'warn' | 'bad'
  text: string
}

export const rootName = (root: RecordingRoot): string =>
  root.recorders.map(({ application, recorder }) => `${application}/${recorder}`).join(', ')

export const percentUsed = (used: number, max: number): number => (max > 0 ? (used / max) * 100 : FULL_AT)

export const toneFor = (percent: number): Tone => (percent >= BAD_AT ? 'bad' : percent >= WARN_AT ? 'warn' : 'ok')

function usage(
  key: Usage['key'], label: string, code: Usage['code'], used: number, max: number | null, show: (value: number) => string,
): Usage {
  if (max === null) {
    return { key, label, code, used, max: 0, limited: false, percent: 0, tone: 'ok', text: `${show(used)} used, No limit` }
  }
  const percent = percentUsed(used, max)
  const text = `${show(used)} of ${show(max)} (${formatPercent(percent)})`
  return { key, label, code, used, max, limited: true, percent, tone: toneFor(percent), text }
}

export const usages = (root: RecordingRoot): Usage[] => [
  usage('bytes', 'Disk space', 'quota_bytes', Number(root.bytes_used), root.max_bytes === null ? null : Number(root.max_bytes), formatBytes),
  usage('files', 'Files', 'quota_files', root.files, root.max_files, formatExact),
]

export function retentionLines(root: RecordingRoot): string[] {
  const age = root.retention_max_age_ms === null
    ? 'Finished recordings are not deleted by age.'
    : `Finished recordings older than ${formatDuration(root.retention_max_age_ms)} are deleted.`
  return [
    RETENTION_WORDS[root.retention],
    age,
    `Evicted so far: ${plural(Number(root.evicted_files), 'file')}, ${formatBytes(root.evicted_bytes)}.`,
  ]
}

const REMEDY = 'Delete recordings or raise the limit in the recorder configuration.'

function alertFor(root: RecordingRoot): QuotaAlert | null {
  const crowded = usages(root).filter((item) => item.limited && item.percent >= BAD_AT)
  const fullest = crowded.reduce<Usage | null>((worst, item) => (worst === null || item.percent > worst.percent ? item : worst), null)
  if (fullest === null) return null
  const name = rootName(root)
  const resource = fullest.label.toLowerCase()
  if (fullest.percent >= FULL_AT) {
    const text = `${name} is full by ${resource}. New recordings are refused and its recorder fails with ${fullest.code}. ${REMEDY}`
    return { name, tone: 'bad', text }
  }
  const text = `${name} is ${formatPercent(fullest.percent)} full by ${resource}. Its recorder will fail soon with ${fullest.code}. ${REMEDY}`
  return { name, tone: 'warn', text }
}

export const quotaAlerts = (roots: readonly RecordingRoot[]): QuotaAlert[] =>
  roots.filter(({ retention }) => retention === 'reject').flatMap((root) => alertFor(root) ?? [])
