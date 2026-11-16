import { isRecord } from '../../../valueGuards'
import { ApiError, apiErrorMessage } from '../../transport'
import { eventCursor, isOperationalEventName, parseEventCursor, parseOperationalEvent } from './decode'
import type { OperationalEvent } from './types'

export interface EventStreamResyncRequired {
  cursor: number
  oldestCursor: number | null
  latestCursor: number
}

export type EventStreamMessage =
  | { type: 'ready'; cursor: number }
  | { type: 'operational'; event: OperationalEvent }
  | { type: 'resync_required'; data: EventStreamResyncRequired }
  | { type: 'shutdown'; reason: 'server_shutdown' }

export interface EventStreamHandlers {
  onReady?: (cursor: number) => void
  onEvent?: (event: OperationalEvent) => void
  onResyncRequired?: (data: EventStreamResyncRequired) => void | Promise<void>
  onShutdown?: () => void
  onError?: (error: unknown) => void
}

export interface EventStreamOptions {
  signal?: AbortSignal
  after?: number
  maxRetries?: number
  retryDelayMs?: number
  maxRetryDelayMs?: number
}

export interface EventStreamClient {
  close: () => void
  closed: Promise<void>
}

const EVENT_STREAM_PATH_V1 = '/api/v1/events/stream'
const EVENT_STREAM_PATH_V2 = '/api/v2/events/stream'
const DEFAULT_EVENT_STREAM_MAX_RETRIES = 5
const DEFAULT_EVENT_STREAM_RETRY_DELAY_MS = 250
const DEFAULT_EVENT_STREAM_MAX_RETRY_DELAY_MS = 5_000

export function parseEventStreamFrame(frame: string): EventStreamMessage | null {
  let eventName = 'message'
  let eventId: string | null = null
  const dataLines: string[] = []
  for (const line of frame.split(/\r\n|\n|\r/)) {
    if (line.startsWith(':')) continue
    const separator = line.indexOf(':')
    const field = separator === -1 ? line : line.slice(0, separator)
    const value = separator === -1
      ? ''
      : line.slice(separator + 1).startsWith(' ')
        ? line.slice(separator + 2)
        : line.slice(separator + 1)
    if (field === 'event') eventName = value
    else if (field === 'id') eventId = value
    else if (field === 'data') dataLines.push(value)
  }
  if (dataLines.length === 0) return null

  let payload: unknown
  try {
    payload = JSON.parse(dataLines.join('\n')) as unknown
  } catch {
    return null
  }

  if (eventName === 'ready') {
    const cursor = eventCursor(payload, 'cursor')
    return cursor === null ? null : { type: 'ready', cursor }
  }
  if (eventName === 'resync_required') {
    if (!isRecord(payload)) return null
    const cursor = eventCursor(payload, 'cursor')
    const oldestCursor = payload.oldestCursor === null
      ? null
      : eventCursor(payload, 'oldestCursor')
    const latestCursor = eventCursor(payload, 'latestCursor')
    return cursor === null || latestCursor === null
      ? null
      : { type: 'resync_required', data: { cursor, oldestCursor, latestCursor } }
  }
  if (eventName === 'shutdown') {
    return payloadIsReason(payload, 'server_shutdown')
      ? { type: 'shutdown', reason: 'server_shutdown' }
      : null
  }
  if (!isOperationalEventName(eventName) || !isRecord(payload)) return null
  const cursor = eventCursor(payload, 'cursor')
  const id = eventId === null ? null : parseEventCursor(eventId)
  const event = parseOperationalEvent(payload, eventName, cursor, id)
  if (event === null) return null
  return {
    type: 'operational',
    event,
  }
}

export function connectEventStream(
  token: string,
  handlers: EventStreamHandlers,
  options: EventStreamOptions = {},
): EventStreamClient {
  const controller = new AbortController()
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let lastEventId: number | null = options.after ?? null
  let streamPath = EVENT_STREAM_PATH_V2
  let closed = false
  let resolveClosed!: () => void
  const closedPromise = new Promise<void>((resolve) => {
    resolveClosed = resolve
  })

  const close = (): void => {
    if (closed) return
    closed = true
    if (retryTimer !== undefined) clearTimeout(retryTimer)
    controller.abort()
    resolveClosed()
  }

  const onExternalAbort = (): void => close()
  if (options.signal) {
    if (options.signal.aborted) close()
    else options.signal.addEventListener('abort', onExternalAbort, { once: true })
  }

  const run = async (): Promise<void> => {
    let retries = 0
    while (!closed) {
      try {
        const headers: Record<string, string> = {
          Accept: 'text/event-stream',
          Authorization: `Bearer ${token}`,
          'Cache-Control': 'no-cache',
        }
        if (lastEventId !== null) headers['Last-Event-ID'] = String(lastEventId)
        const response = await fetch(streamPath, {
          cache: 'no-store',
          headers,
          signal: controller.signal,
        })
        if (response.status === 404 && streamPath === EVENT_STREAM_PATH_V2) {
          streamPath = EVENT_STREAM_PATH_V1
          continue
        }
        if (!response.ok) {
          throw await eventStreamResponseError(response)
        }
        const contentType = response.headers.get('content-type')?.toLowerCase() ?? ''
        if (!contentType.startsWith('text/event-stream')) {
          throw new Error('The event stream returned an invalid content type.')
        }
        if (!response.body) throw new Error('The event stream returned no body.')
        await consumeEventStream(response.body, async (message) => {
          if (message.type === 'ready') {
            lastEventId = message.cursor
            handlers.onReady?.(message.cursor)
          } else if (message.type === 'operational') {
            lastEventId = message.event.cursor
            handlers.onEvent?.(message.event)
          } else if (message.type === 'resync_required') {
            lastEventId = message.data.latestCursor
            await handlers.onResyncRequired?.(message.data)
          } else {
            handlers.onShutdown?.()
          }
        }, controller.signal)
        if (closed) return
        if (lastMessageWasShutdown) return
        if (lastMessageWasResync) continue
        throw new EventStreamDisconnectedError()
      } catch (error) {
        if (closed || isAbortError(error)) return
        handlers.onError?.(error)
        if (!isRetryableEventStreamError(error) || retries >= (options.maxRetries ?? DEFAULT_EVENT_STREAM_MAX_RETRIES)) {
          return
        }
        const baseDelay = options.retryDelayMs ?? DEFAULT_EVENT_STREAM_RETRY_DELAY_MS
        const maxDelay = options.maxRetryDelayMs ?? DEFAULT_EVENT_STREAM_MAX_RETRY_DELAY_MS
        const delay = Math.min(maxDelay, baseDelay * 2 ** retries)
        retries += 1
        await waitForRetry(delay, controller.signal, () => {
          retryTimer = undefined
        })
      }
    }
  }

  let lastMessageWasShutdown = false
  let lastMessageWasResync = false
  void run().finally(() => {
    if (options.signal) options.signal.removeEventListener('abort', onExternalAbort)
    close()
  })
  return { close, closed: closedPromise }

  async function consumeEventStream(
    body: ReadableStream<Uint8Array>,
    onMessage: (message: EventStreamMessage) => void | Promise<void>,
    signal: AbortSignal,
  ): Promise<void> {
    const reader = body.getReader()
    const decoder = new TextDecoder()
    const parser = new EventStreamParser()
    lastMessageWasShutdown = false
    lastMessageWasResync = false
    while (!signal.aborted) {
      const { done, value } = await reader.read()
      if (done) break
      for (const message of parser.push(decoder.decode(value, { stream: true }))) {
        await dispatch(message)
        if (lastMessageWasShutdown) return
      }
    }
    for (const message of parser.push(decoder.decode())) await dispatch(message)

    async function dispatch(message: EventStreamMessage): Promise<void> {
      await onMessage(message)
      if (message.type === 'shutdown') lastMessageWasShutdown = true
      if (message.type === 'resync_required') lastMessageWasResync = true
    }
  }

  function waitForRetry(
    delay: number,
    signal: AbortSignal,
    onSettled: () => void,
  ): Promise<void> {
    return new Promise((resolve) => {
      if (signal.aborted || closed) {
        onSettled()
        resolve()
        return
      }
      retryTimer = setTimeout(() => {
        onSettled()
        resolve()
      }, delay)
      signal.addEventListener('abort', () => {
        if (retryTimer !== undefined) clearTimeout(retryTimer)
        onSettled()
        resolve()
      }, { once: true })
    })
  }
}

class EventStreamDisconnectedError extends Error {
  constructor() {
    super('The event stream ended before shutdown.')
    this.name = 'EventStreamDisconnectedError'
  }
}

class EventStreamParser {
  private buffer = ''
  private frame: string[] = []

  push(chunk: string): EventStreamMessage[] {
    this.buffer += chunk
    const messages: EventStreamMessage[] = []
    while (true) {
      const newline = this.nextNewline()
      if (newline === null) break
      const line = this.buffer.slice(0, newline.index)
      this.buffer = this.buffer.slice(newline.nextIndex)
      if (line === '') {
        const message = parseEventStreamFrame(this.frame.join('\n'))
        if (message) messages.push(message)
        this.frame = []
      } else {
        this.frame.push(line)
      }
    }
    return messages
  }

  private nextNewline(): { index: number; nextIndex: number } | null {
    const lineFeed = this.buffer.indexOf('\n')
    const carriageReturn = this.buffer.indexOf('\r')
    if (lineFeed === -1 && carriageReturn === -1) return null
    const index = lineFeed === -1
      ? carriageReturn
      : carriageReturn === -1
        ? lineFeed
        : Math.min(lineFeed, carriageReturn)
    if (this.buffer[index] === '\r' && index + 1 === this.buffer.length) return null
    return {
      index,
      nextIndex: this.buffer[index] === '\r' && this.buffer[index + 1] === '\n'
        ? index + 2
        : index + 1,
    }
  }
}

async function eventStreamResponseError(response: Response): Promise<ApiError> {
  let payload: unknown = null
  try {
    payload = await response.json() as unknown
  } catch {
    // The status remains the useful contract when an error body is not JSON.
  }
  return new ApiError(
    response.status,
    apiErrorMessage(payload) ?? `Event stream returned status ${response.status}`,
    payload,
  )
}

function payloadIsReason(value: unknown, reason: string): boolean {
  return isRecord(value) && value.reason === reason
}

function isAbortError(error: unknown): boolean {
  return error instanceof Error && error.name === 'AbortError'
}

function isRetryableEventStreamError(error: unknown): boolean {
  return error instanceof TypeError || error instanceof EventStreamDisconnectedError
}
