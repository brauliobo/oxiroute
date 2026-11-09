import type { StoreStatus } from './store'
import type { FailureKind, PollFailure } from './poll'

// The text above the dashboard when the data is not simply live. It never carries a ticking value (the age lives in
// the status line), because it is announced to screen readers whenever it changes.

export interface Banner {
  tone: 'warn' | 'bad'
  text: string
  action?: 'retry' | 'reload'
}

const REASONS: Readonly<Record<Exclude<FailureKind, 'http' | 'schema'>, string>> = {
  unavailable: 'the server cannot sample statistics right now',
  network: 'the server could not be reached',
  decode: 'the response was not valid statistics',
}

const reason = ({ kind, message }: PollFailure) => (kind === 'http' || kind === 'schema' ? message : REASONS[kind])
const seconds = ({ retryInMs }: PollFailure) => (retryInMs === undefined ? undefined : `${Math.ceil(retryInMs / 1000)} s`)

export function statusBanner(status: StoreStatus, failure: PollFailure | undefined): Banner | undefined {
  if (status === 'stale') {
    return { tone: 'warn', text: 'The data is stale: it has not refreshed for a while, so these are the last values received.' }
  }
  if (status === 'unsupported') {
    return {
      tone: 'bad', action: 'reload',
      text: 'This page does not understand the statistics format of this server. Reload the page to get the matching version.',
    }
  }
  if (!failure) return undefined
  const wait = seconds(failure)
  if (status === 'error') {
    return { tone: 'bad', action: 'retry', text: `Statistics are unavailable (${reason(failure)}).${wait ? ` Retrying in ${wait}.` : ''}` }
  }
  if (status !== 'retrying') return undefined
  const next = wait ? `; retrying in ${wait}` : ''
  return {
    tone: 'warn', action: 'retry',
    text: `Could not refresh (${reason(failure)}). Showing the last values${next} (attempt ${failure.attempts}).`,
  }
}
