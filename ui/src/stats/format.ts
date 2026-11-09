// Display formatting without Intl or toLocaleString: the same input gives the same text on every machine, locale and
// time zone, which keeps tests and screenshots stable. Counts and sizes are shortened to two or three significant
// digits (exact values belong in a tooltip: formatExact). Durations truncate rather than round.

export const UNKNOWN = '—'

const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB', 'EiB']
const COUNT_UNITS = ['', 'k', 'M', 'G', 'T', 'P', 'E']

type Numeric = number | bigint

/** One decimal below 10, none above; a trailing `.0` is dropped. */
const digits = (value: number) => (value < 10 ? value.toFixed(1).replace(/\.0$/, '') : value.toFixed(0))

function scaled(value: Numeric, base: number, units: readonly string[]): [text: string, unit: string] {
  const magnitude = Math.abs(Number(value))
  let index = 0
  let scale = magnitude
  while (scale >= base && index < units.length - 1) {
    scale /= base
    index++
  }
  let text = digits(scale)
  if (Number(text) >= base && index < units.length - 1) {
    index++
    text = digits(scale / base)
  }
  return [`${value < 0 ? '-' : ''}${text}`, units[index]!]
}

export function formatBytes(value: Numeric): string {
  const [text, unit] = scaled(value, 1024, BYTE_UNITS)
  return `${text} ${unit}`
}

export function formatCount(value: Numeric): string {
  const [text, unit] = scaled(value, 1000, COUNT_UNITS)
  return `${text}${unit}`
}

/** Digits grouped by thousands with commas. */
export function formatExact(value: Numeric): string {
  return value.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',')
}

export function formatRate(perSecond: number): string {
  if (perSecond === 0) return '0/s'
  if (perSecond < 0.01) return '<0.01/s'
  return `${perSecond < 10 ? Number(perSecond.toFixed(2)).toString() : formatCount(Math.round(perSecond))}/s`
}

export const formatByteRate = (bytesPerSecond: number) => `${formatBytes(bytesPerSecond)}/s`

const MINUTE = 60
const HOUR = 3600
const DAY = 86_400

export function formatDuration(ms: number): string {
  if (ms < 0) return `-${formatDuration(-ms)}`
  if (ms < 10) return `${Math.floor(ms * 10) / 10} ms`
  if (ms < 1000) return `${Math.floor(ms)} ms`
  const seconds = Math.floor(ms / 1000)
  if (seconds < 10) return `${Math.floor(ms / 100) / 10} s`
  if (seconds < MINUTE) return `${seconds} s`
  if (seconds < HOUR) return `${Math.floor(seconds / MINUTE)}m ${seconds % MINUTE}s`
  if (seconds < DAY) return `${Math.floor(seconds / HOUR)}h ${Math.floor((seconds % HOUR) / MINUTE)}m`
  return `${Math.floor(seconds / DAY)}d ${Math.floor((seconds % DAY) / HOUR)}h`
}

export const formatAge = (ms: number) => (ms < 1000 ? 'just now' : `${formatDuration(ms)} ago`)

const two = (value: number) => String(value).padStart(2, '0')

/** `YYYY-MM-DD HH:MM:SS` in UTC (`offsetMinutes` 0) or at a fixed offset east of UTC, suffixed with the zone. */
export function formatTimestamp(unixMs: number, offsetMinutes = 0): string {
  const text = new Date(unixMs + offsetMinutes * 60_000).toISOString().replace('T', ' ').slice(0, 19)
  const magnitude = Math.abs(offsetMinutes)
  const zone = offsetMinutes === 0 ? 'UTC' : `${offsetMinutes < 0 ? '-' : '+'}${two(Math.floor(magnitude / 60))}:${two(magnitude % 60)}`
  return `${text} ${zone}`
}

export const formatPercent = (value: number) => `${Number(value.toFixed(1))}%`

const graphemes = new Intl.Segmenter(undefined, { granularity: 'grapheme' })

/** Cuts to `max` user-perceived characters, ellipsis included, never inside a surrogate pair or an emoji sequence. */
export function truncate(text: string, max: number): string {
  if (max < 1) throw new RangeError(`max must be at least 1: ${max}`)
  const parts = Array.from(graphemes.segment(text), (part) => part.segment)
  return parts.length <= max ? text : `${parts.slice(0, max - 1).join('')}…`
}
