import { describe, expect, it } from 'vitest'
import { statusBanner } from './banner'
import type { PollFailure } from './poll'

const failure = (kind: PollFailure['kind'], extra: Partial<PollFailure> = {}): PollFailure =>
  ({ kind, message: 'boom', attempts: 3, retryInMs: 4000, ...extra })

describe('statusBanner', () => {
  it('has nothing to say while loading, live or paused', () => {
    for (const status of ['loading', 'live', 'paused'] as const) expect(statusBanner(status, undefined)).toBeUndefined()
  })

  it('warns that stale values are the last ones received, without a ticking age', () => {
    const banner = statusBanner('stale', undefined)!
    expect(banner).toMatchObject({ tone: 'warn' })
    expect(banner.text).toBe('The data is stale: it has not refreshed for a while, so these are the last values received.')
  })

  it('names the reason, the wait and the attempt while retrying with data on screen', () => {
    expect(statusBanner('retrying', failure('network'))).toEqual({
      tone: 'warn', action: 'retry',
      text: 'Could not refresh (the server could not be reached). Showing the last values; retrying in 4 s (attempt 3).',
    })
    expect(statusBanner('retrying', failure('unavailable', { retryInMs: 1500 }))!.text)
      .toContain('the server cannot sample statistics right now). Showing the last values; retrying in 2 s')
  })

  it('shows unavailability instead of numbers when nothing was ever loaded', () => {
    expect(statusBanner('error', failure('http', { message: 'unexpected HTTP status 500' }))).toEqual({
      tone: 'bad', action: 'retry', text: 'Statistics are unavailable (unexpected HTTP status 500). Retrying in 4 s.',
    })
    expect(statusBanner('error', failure('decode'))!.text).toContain('the response was not valid statistics')
  })

  it('asks for a reload when the page and the server disagree on the format', () => {
    expect(statusBanner('unsupported', failure('schema', { retryInMs: undefined }))).toEqual({
      tone: 'bad', action: 'reload',
      text: 'This page does not understand the statistics format of this server. Reload the page to get the matching version.',
    })
  })

  it('omits the wait when polling is not going to retry', () => {
    expect(statusBanner('error', failure('network', { retryInMs: undefined }))!.text)
      .toBe('Statistics are unavailable (the server could not be reached).')
  })
})
