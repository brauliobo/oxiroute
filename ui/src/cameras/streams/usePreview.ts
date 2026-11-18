import { onBeforeUnmount, shallowRef } from 'vue'
import { ApiError, createMediaTicket } from '../../api'
import type { MediaTicket } from '../../api'
import { presentApiError } from '../../formatters'
import type { Camera } from './model'

export const RENEW_BEFORE_EXPIRY_MS = 30_000

export type PreviewState =
  | { kind: 'starting' }
  | { kind: 'playing'; src: string }
  | { kind: 'unavailable'; message: string }
  | { kind: 'failed'; message: string }

const NO_OUTPUT = 'This application has no HLS output, so the stream cannot be previewed here.'
const DASH_ONLY = 'This application only publishes DASH, which browsers cannot play natively.'
const PLAYBACK_FAILED = 'The preview could not be played. The stream may have just started or stopped.'

const isHls = (ticket: MediaTicket) => new URL(ticket.path, 'http://localhost').pathname.endsWith('.m3u8')

export function usePreview(camera: () => Camera, token: () => string, onUnauthorized: () => void) {
  const state = shallowRef<PreviewState>({ kind: 'starting' })
  let renewal: number | undefined
  let attempt = 0

  const stopRenewal = () => window.clearTimeout(renewal)

  function scheduleRenewal({ ttlSeconds }: MediaTicket): void {
    stopRenewal()
    const lifetimeMs = ttlSeconds * 1000
    renewal = window.setTimeout(() => void start(), Math.max(lifetimeMs / 2, lifetimeMs - RENEW_BEFORE_EXPIRY_MS))
  }

  function failure(error: unknown): PreviewState {
    if (error instanceof ApiError && error.status === 401) onUnauthorized()
    if (error instanceof ApiError && error.code === 'media_not_found') return { kind: 'unavailable', message: NO_OUTPUT }
    return { kind: 'failed', message: presentApiError(error, 'The preview ticket could not be requested.') }
  }

  async function start(): Promise<void> {
    const current = ++attempt
    const { service, application, name } = camera()
    try {
      const ticket = await createMediaTicket(service, application, name, token())
      if (current !== attempt) return
      if (!isHls(ticket)) {
        state.value = { kind: 'unavailable', message: DASH_ONLY }
        return
      }
      state.value = { kind: 'playing', src: ticket.path }
      scheduleRenewal(ticket)
    } catch (error) {
      if (current === attempt) state.value = failure(error)
    }
  }

  function retry(): void {
    state.value = { kind: 'starting' }
    void start()
  }

  function playbackFailed(): void {
    stopRenewal()
    attempt++
    state.value = { kind: 'failed', message: PLAYBACK_FAILED }
  }

  onBeforeUnmount(() => {
    attempt++
    stopRenewal()
  })

  return { state, start, retry, playbackFailed }
}
