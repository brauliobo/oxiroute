import type { Tone } from '#design/badge/tone'
import type { SelectOption } from '#design/field/types'
import type { AuditCategory, AuditComponentState, AuditResult } from '../api'
import { sentence } from '../events/format'

const CATEGORIES: readonly AuditCategory[] = ['reload', 'import', 'certificate', 'control']
const RESULTS: readonly AuditResult[] = ['requested', 'succeeded', 'failed', 'rejected', 'conflict', 'partial', 'degraded']

const options = (all: string, values: readonly string[]): SelectOption[] =>
  [{ value: '', label: all }, ...values.map((value) => ({ value, label: sentence(value) }))]

export const CATEGORY_OPTIONS = options('All categories', CATEGORIES)
export const RESULT_OPTIONS = options('All results', RESULTS)

const RESULT_TONES: Readonly<Record<AuditResult, Tone>> = {
  succeeded: 'ok',
  requested: 'info',
  partial: 'warn',
  degraded: 'warn',
  failed: 'bad',
  rejected: 'bad',
  conflict: 'bad',
}

export const resultTone = (result: AuditResult): Tone => RESULT_TONES[result]

export type StoreState = AuditComponentState

const STORE_BADGES: Readonly<Record<StoreState, { tone: Tone; label: string }>> = {
  healthy: { tone: 'ok', label: 'Healthy' },
  degraded: { tone: 'warn', label: 'Degraded' },
  memory: { tone: 'warn', label: 'Memory only' },
}

export const storeBadge = (state: StoreState): { tone: Tone; label: string } => STORE_BADGES[state]

export type ConnectionState = 'unavailable' | 'ready' | 'degraded'

const CONNECTION_BADGES: Readonly<Record<ConnectionState, { tone: Tone; label: string }>> = {
  ready: { tone: 'ok', label: 'Connected' },
  degraded: { tone: 'warn', label: 'Degraded' },
  unavailable: { tone: 'bad', label: 'Unavailable' },
}

export const connectionBadge = (state: ConnectionState): { tone: Tone; label: string } => CONNECTION_BADGES[state]

const CHECKS: Readonly<Record<string, string>> = {
  invalid_limits: 'Check that the audit limits are positive and consistent.',
  root_open_failed: 'Check that the audit directory and its parents are real directories the daemon may create and open.',
  root_mode_unsafe: 'Check that the audit directory is a real directory with mode 0700.',
  unsafe_entry: 'Check that the audit files are regular files with mode 0600.',
  io_failed: 'Check that the audit directory is readable and writable by the daemon and that the disk has free space.',
  record_too_large: 'A record exceeded the maximum record size. Check the audit record limit.',
}

const GENERIC_CHECK = 'Check the permissions and free space of the audit directory, then the daemon log.'

export const degradedCheck = (reason: string | undefined): string => (reason && CHECKS[reason]) || GENERIC_CHECK
