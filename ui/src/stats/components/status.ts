// Status words of every kind of row, reduced to a tone. A badge always shows the word and a shape per tone; the colour
// only repeats them.

export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral'

const TONES: Readonly<Record<string, Tone>> = {
  up: 'ok', healthy: 'ok', listening: 'ok', ready: 'ok', ok: 'ok',
  degraded: 'warn', drain: 'warn', stopped: 'warn', soon: 'warn',
  down: 'bad', ejected: 'bad', failed: 'bad', expired: 'bad', unhealthy: 'bad',
  maint: 'info', maintenance: 'info',
}

export const toneOf = (status: string): Tone => TONES[status] ?? 'neutral'

/** One filled shape per tone (16x16, even-odd), so a badge differs in form and not only in colour. */
export const ICON_PATHS: Readonly<Record<Tone, string>> = {
  ok: 'M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1Zm3.2 4.6-4.2 4.2-2.2-2.2.9-.9 1.3 1.3 3.3-3.3Z',
  warn: 'M8 1.5 15 14H1Zm-.7 4.5v4h1.4V6Zm0 5v1.4h1.4V11Z',
  bad: 'M5 1h6l4 4v6l-4 4H5l-4-4V5Zm.2 3.3L7.1 8l-2 2.8.9.9L8 8.9l2 2.8.9-.9L8.9 8l2-3.7-.9-.9L8 7.1 6 3.4Z',
  info: 'M8 1l7 7-7 7-7-7Zm-.7 6v4.5h1.4V7Zm0-2.4V6h1.4V4.6Z',
  neutral: 'M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1Zm0 1.5a5.5 5.5 0 1 1 0 11 5.5 5.5 0 0 1 0-11ZM5 7.3v1.4h6V7.3Z',
}
