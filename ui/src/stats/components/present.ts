import {
  UNKNOWN, formatAge, formatByteRate, formatBytes, formatCount, formatDuration, formatExact, formatPercent, formatRate,
  formatTimestamp,
} from '../format'
import type { CertificateRow } from '../model'
import type { Quantile } from '../percentiles'

// Text for table cells: every function returns the em dash for an absent value, never a made-up zero.

type Numeric = number | bigint
const orUnknown = <T>(value: T | undefined, format: (value: T) => string) => (value === undefined ? UNKNOWN : format(value))

export const count = (value?: Numeric) => orUnknown(value, formatCount)
export const exact = (value?: Numeric) => orUnknown(value, formatExact)
export const bytes = (value?: Numeric) => orUnknown(value, formatBytes)
export const rate = (value?: number) => orUnknown(value, formatRate)
export const byteRate = (value?: number) => orUnknown(value, formatByteRate)
export const duration = (ms?: number) => orUnknown(ms, formatDuration)
export const percent = (value?: number) => orUnknown(value, formatPercent)
export const text = (value?: string | number) => (value === undefined || value === '' ? UNKNOWN : String(value))
export const yesNo = (value?: boolean) => orUnknown(value, (flag) => (flag ? 'yes' : 'no'))

/** Time since `unixMs` at the sample time; the server's clock on both sides, so the browser clock plays no part. */
export const since = (sampledAtUnixMs: number, unixMs?: number) =>
  orUnknown(unixMs, (at) => formatAge(Math.max(0, sampledAtUnixMs - at)))
export const timestamp = (unixMs?: number) => orUnknown(unixMs, (at) => formatTimestamp(at))

/** `cur / max`; an absent limit is shown as unknown, not as unlimited. */
export const sessions = (current: number, max?: number) => `${formatCount(current)} / ${count(max)}`

/** A percentile is the upper bound of its bucket, so it reads "at most"; beyond the last bound it reads "more than". */
export const quantileText = (value?: Quantile) =>
  orUnknown(value, ({ ms, overflow }) => `${overflow ? '> ' : ''}${formatDuration(ms)}`)
export const quantileTitle = (value?: Quantile) =>
  value === undefined ? 'no observations yet' : `${value.overflow ? 'more than' : 'at most'} ${formatDuration(value.ms)} (bucket bound)`

export const RESPONSE_CLASSES = ['1xx', '2xx', '3xx', '4xx', '5xx'] as const
export type ResponseClass = (typeof RESPONSE_CLASSES)[number]
export interface ResponseShare { class: ResponseClass; count: bigint; share: number }

/** The response classes of a row as shares of all classified responses; empty while there are none. */
export function responseShares(responses: Readonly<Record<ResponseClass, bigint>>): ResponseShare[] {
  const total = RESPONSE_CLASSES.reduce((sum, name) => sum + responses[name], 0n)
  if (total === 0n) return []
  return RESPONSE_CLASSES.map((name) => ({
    class: name, count: responses[name], share: Number((responses[name] * 10_000n) / total) / 100,
  }))
}

/** A DOM id for a row key: keys carry `:`, `%` and arbitrary names, ids and fragments must stay plain. */
export const anchorId = (key: string) => key.replace(/[^A-Za-z0-9-]/g, (char) => `_${char.charCodeAt(0).toString(16)}`)

/** A labelled value of a details list; an absent `value` renders as the em dash. */
export interface Detail { label: string; value?: string | number; title?: string }

/** A wire identifier (`segment_prefix`, `static_files`) as words. */
export const words = (identifier: string) => identifier.replace(/_/g, ' ')

const CERTIFICATE_LABELS: Readonly<Record<CertificateRow['severity'], string>> = {
  ok: 'VALID', soon: 'EXPIRES SOON', expired: 'EXPIRED', unknown: 'UNKNOWN',
}
export const certificateLabel = (severity: CertificateRow['severity']) => CERTIFICATE_LABELS[severity]

/** Validity left (`in 3d 4h`) or how long ago it ended (`expired 2d 1h ago`). */
export const remaining = (ms?: number) =>
  orUnknown(ms, (left) => (left < 0 ? `expired ${formatDuration(-left)} ago` : `in ${formatDuration(left)}`))
