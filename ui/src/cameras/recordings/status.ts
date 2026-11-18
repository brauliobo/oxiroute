import type { Tone } from '#design/badge/tone'
import type { RecordingStatus } from '../../api'

export interface StatusWord {
  label: string
  tone: Tone
  note: string | null
}

export const STATUSES: readonly RecordingStatus[] = ['recording', 'complete', 'partial']

export const STATUS_WORDS: Readonly<Record<RecordingStatus, StatusWord>> = {
  recording: { label: 'RECORDING', tone: 'info', note: 'Being written now.' },
  complete: { label: 'COMPLETE', tone: 'ok', note: null },
  partial: {
    label: 'PARTIAL',
    tone: 'warn',
    note: 'The recorder was cut short, for example by a crash or a failed recorder. The file ends at the last whole frame.',
  },
}

export const STATUS_RANK: Readonly<Record<RecordingStatus, number>> = { recording: 0, partial: 1, complete: 2 }

export const asStatus = (value: string): RecordingStatus | undefined => STATUSES.find((status) => status === value)
