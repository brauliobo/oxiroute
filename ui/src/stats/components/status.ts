import type { Tone } from '#design/badge/tone'

const TONES: Readonly<Record<string, Tone>> = {
  up: 'ok', healthy: 'ok', listening: 'ok', ready: 'ok', ok: 'ok',
  degraded: 'warn', drain: 'warn', stopped: 'warn', soon: 'warn',
  down: 'bad', ejected: 'bad', failed: 'bad', expired: 'bad', unhealthy: 'bad',
  maint: 'info', maintenance: 'info',
}

export const toneOf = (status: string): Tone => TONES[status] ?? 'neutral'
