import type {
  CanonicalConfig,
  ConfigDiagnostic,
  ConfigFormat,
  ConfigRequest,
  ConfigSaveResponse,
  ConfigSnapshot,
  ConfigValidationResponse,
  HttpRetryTrigger,
  ListenerBind,
  ListenerProtocol,
  UpstreamAlgorithm,
  UpstreamEndpoint,
} from './config'
import { isCanonicalConfig, isConfigDiagnostic } from './config'
import {
  decimalString,
  finiteNumber,
  isRecord,
  nullableSafeInteger,
  nullableString,
  safeInteger,
} from './valueGuards'

export interface RtmpCapabilities {
  live_ingest: boolean
  manual_recording: boolean
}

export interface TrackSnapshot {
  codec_id: number | null
  codec_fourcc: string | null
  codec_name: string | null
  recording_supported: boolean
  payload_bytes: string
  last_rtmp_timestamp_ms: number | null
  last_observed_at_unix_ms: number | null
}

export interface RecorderPhase {
  state: 'idle' | 'starting' | 'recording' | 'stopping' | 'failed'
  operation_id?: string
  started_at_unix_ms?: number
  code?: string
}

export interface RecorderSnapshot {
  id: string
  name: string | null
  manual: boolean
  phase: RecorderPhase
  changed_at_unix_ms: number
  bytes_written: string
  current_relative_name: string | null
  published_but_not_durable_relative_name: string | null
  segments_started: string
  segments_completed: string
  discontinuities: string
  last_completed_relative_name: string | null
  recoverable_partial_name: string | null
}

export interface RelaySnapshot {
  id: string
  destination: { address: string; application: string; stream_name: string }
  phase: 'connecting' | 'publishing' | 'backoff' | 'stopped'
  last_failure: 'connect' | 'handshake' | 'session' | 'transport' | 'thread' | null
  queue_messages: number
  queue_bytes: string
  connection_attempts: string
  connections: string
  reconnects: string
  events_enqueued: string
  events_sent: string
  events_dropped: string
  payload_bytes_sent: string
}

export interface StreamSnapshot {
  id: string
  revision: string
  server_id: string
  application: string
  name: string
  created_at_unix_ms: number
  publisher: {
    session_id: string
    attached_at_unix_ms: number
  } | null
  subscriber_count: number
  media: {
    audio: TrackSnapshot
    video: TrackSnapshot
    fanout_payload_bytes: string
  }
  relays: RelaySnapshot[]
  recording_supported: boolean
  manual_recording: boolean
  recorders: RecorderSnapshot[]
}

export interface RtmpCatalog {
  revision: string
  as_of_unix_ms: number
  capabilities: RtmpCapabilities
  streams: StreamSnapshot[]
}

export interface MonitoringProcess {
  activeConnections: number
  administrativeState: AdministrativeState
  cpuPercent: number | null
  maxConnections: number | null
  rejectedConnections: string
  retryAttempts: string
  residentMemoryBytes: number
  virtualMemoryBytes: number
  threadCount: number
  openFileDescriptors: number
}

export interface MonitoringHost {
  loadAverage1m: number
  loadAverage5m: number
  loadAverage15m: number
  totalMemoryBytes: number
  availableMemoryBytes: number
}

export interface MonitoringTraffic {
  acceptedConnections: string
  rejectedConnections: string
  activeConnections: number
  bytesReceived: string
  bytesSent: string
}

export type ListenerRuntimeState = 'configured' | 'listening' | 'stopped' | 'failed'
export type AdministrativeState = 'ready' | 'drain' | 'maintenance'

export interface MonitoringListener extends MonitoringTraffic {
  administrativeState: AdministrativeState
  name: string
  protocol: ListenerProtocol
  bind: string
  maxConnections: number | null
  state: ListenerRuntimeState
}

export interface MonitoringRtmp {
  activeStreams: number
  publishers: number
  subscribers: number
  mediaPayloadBytesReceived: string
  recordingSupported: boolean
  manualRecording: boolean
  recorderBytesWritten: string
  recorderSegmentsStarted: string
  recorderSegmentsCompleted: string
  recorderDiscontinuities: string
  relayConnectionAttempts: string
  relayConnections: string
  relayReconnects: string
  relayEventsSent: string
  relayEventsDropped: string
  relayPayloadBytesSent: string
  relays: MonitoringRelay[]
  recorders: MonitoringRecorder[]
}

export interface MonitoringRelay {
  streamId: string
  relayId: string
  address: string
  application: string
  streamName: string
  phase: RelaySnapshot['phase']
  lastFailure: RelaySnapshot['last_failure']
  queueMessages: number
  queueBytes: string
  connectionAttempts: string
  connections: string
  reconnects: string
  eventsSent: string
  eventsDropped: string
  payloadBytesSent: string
}

export interface MonitoringRecorder {
  streamId: string
  recorderId: string
  name: string | null
  manual: boolean
  phase: RecorderPhase['state']
  bytesWritten: string
  segmentsStarted: string
  segmentsCompleted: string
  discontinuities: string
  currentRelativeName: string | null
  lastCompletedRelativeName: string | null
  recoverablePartialName: string | null
  publishedButNotDurableRelativeName: string | null
}

export interface CertbotCertificateSnapshot {
  name: string
  activeArchiveRevision: number
  activeContentRevision: string
  expiresAt: string
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface CertbotWatcherSnapshot {
  health: 'healthy' | 'degraded' | 'stopped'
  coalescedEvents: string
  ignoredAccessEvents: string
  backendErrors: string
  watchRecoveries: string
  watchRefreshes: string
  rescans: string
  periodicRescans: string
  reconciliationFailures: string
}

export type EndpointHealthState = 'unchecked' | 'unknown' | 'healthy' | 'unhealthy'
export type HealthFailure = 'timeout' | 'connect_failed' | 'unexpected_status' | 'protocol_error'
export type HealthOverride = 'auto' | 'up' | 'down'

export interface MonitoringPoolEndpoint {
  activeConnections: string
  administrativeState: AdministrativeState
  address: string
  checksEnabled: boolean
  checksRunning: boolean
  configuredMaxConnections: number | null
  healthOverride: HealthOverride
  maxConnections: number | null
  name: string
  state: EndpointHealthState
  lastCheckedAtUnixMs: number | null
  lastTransitionAtUnixMs: number | null
  successfulChecks: string
  failedChecks: string
  consecutiveSuccesses: string
  consecutiveFailures: string
  lastFailure: HealthFailure | null
}

export interface MonitoringPool {
  name: string
  algorithm: UpstreamAlgorithm
  availableEndpoints: number
  totalEndpoints: number
  unavailableSelections: string
  queued: number
  queuedTotal: string
  queueTimeouts: string
  queueCancellations: string
  endpoints: MonitoringPoolEndpoint[]
}

export interface MonitoringSnapshot {
  sampledAtUnixMs: number
  uptimeMs: number
  process: MonitoringProcess
  host: MonitoringHost
  traffic: MonitoringTraffic
  listeners: MonitoringListener[]
  upstreamPools: MonitoringPool[]
  certbotCertificates: CertbotCertificateSnapshot[]
  certbotWatcher: CertbotWatcherSnapshot | null
  rtmp: MonitoringRtmp
}

export type TopologyNodeKind =
  | 'listener'
  | 'forward_proxy_listener'
  | 'forward_proxy_service'
  | 'rtmp_listener'
  | 'tls_profile'
  | 'certificate'
  | 'http_service'
  | 'http_route'
  | 'l4_service'
  | 'upstream_pool'
  | 'endpoint'

export type TopologyEdgeKind =
  | 'dispatch_service'
  | 'service_route'
  | 'route_pool'
  | 'service_pool'
  | 'pool_endpoint'
  | 'listener_tls'
  | 'tls_certificate'

export interface TopologyProxyActionSummary extends Record<string, unknown> {
  type: 'proxy'
  upstreamPool: string
  upstreamHost: 'preserve_incoming' | 'endpoint' | 'literal'
  requestHeaderMutationCount: number
  responseHeaderMutationCount: number
  cookiePathRewriteCount: number
  retry: {
    maxRetries: number
    triggers: HttpRetryTrigger[]
    target: 'same_server' | 'next_server'
    delayMs: number
    finalRedispatch: boolean
  }
}

export interface TopologyFixedResponseActionSummary extends Record<string, unknown> {
  type: 'fixed_response'
  status: number
  bodyBytes: number
  headerCount: number
}

export interface TopologyRedirectActionSummary extends Record<string, unknown> {
  type: 'redirect'
  status: number
  locationType: 'literal' | 'request_template'
}

export interface TopologyStaticFilesActionSummary extends Record<string, unknown> {
  type: 'static_files'
  indexFiles: string[]
  spaFallback: boolean
}

export type TopologyHttpActionSummary =
  | TopologyProxyActionSummary
  | TopologyFixedResponseActionSummary
  | TopologyRedirectActionSummary
  | TopologyStaticFilesActionSummary

export interface TopologyNodeAttributes extends Record<string, unknown> {
  bind?: ListenerBind
  type?: UpstreamEndpoint['type']
  address?: string
  host?: string | null | { kind: string; value: string }
  port?: number
  path?: string | { kind: string; value: string }
  maxConnections?: number | null
  maxRequestBodyBytes?: number | null
  action?: TopologyHttpActionSummary
}

export interface TopologyNode {
  id: string
  kind: TopologyNodeKind
  name: string
  configPath: string
  attributes: TopologyNodeAttributes
}

export interface TopologyEdge {
  id: string
  kind: TopologyEdgeKind
  source: string
  target: string
  configPath: string
}

export type TopologyRuntimeStatus = 'active' | 'starting' | 'degraded'
export type TopologyListenerState = ListenerRuntimeState
export type TopologyPoolState = 'available' | 'degraded' | 'unavailable'
export type TopologyRuntimeState = TopologyListenerState | TopologyPoolState | EndpointHealthState

export interface TopologyRuntimeOverlay {
  nodeId: string
  state: TopologyRuntimeState
  metrics: TopologyRuntimeMetrics
}

export interface TopologyRuntimeMetrics extends Record<string, unknown> {
  activeConnections?: number | string
  acceptedConnections?: string
  rejectedConnections?: string
  bytesReceived?: string
  bytesSent?: string
  availableEndpoints?: number
  totalEndpoints?: number
  unavailableSelections?: string
  queued?: number
  queuedTotal?: string
  queueTimeouts?: string
  queueCancellations?: string
  maxConnections?: number | null
  lastCheckedAtUnixMs?: number | null
  lastTransitionAtUnixMs?: number | null
  successfulChecks?: string
  failedChecks?: string
  consecutiveSuccesses?: string
  consecutiveFailures?: string
  lastFailure?: HealthFailure | null
}

export interface TopologySnapshot {
  schemaVersion: 1
  state: {
    config: 'active'
    runtime: TopologyRuntimeStatus
    sampledAtUnixMs: number
  }
  nodes: TopologyNode[]
  edges: TopologyEdge[]
  overlays: TopologyRuntimeOverlay[]
}

export interface CandidateTopologySnapshot extends Omit<TopologySnapshot, 'state'> {
  state: {
    config: 'candidate'
    runtime: 'not_active'
    sampledAtUnixMs: number
  }
}

export type OperationalEventName =
  | 'generation_prepare'
  | 'generation_activate'
  | 'generation_rollback'
  | 'generation_start'
  | 'process_shutdown'
  | 'listener_administrative_state'
  | 'pool_administrative_state'
  | 'server_update'
  | 'unknown'

export type OperationalEventOutcome =
  | 'prepared'
  | 'rejected'
  | 'activated'
  | 'quarantined'
  | 'requested'
  | 'applied'
  | 'unknown'

export interface OperationalEvent {
  cursor: number
  timestampUnixMs: number | null
  event: OperationalEventName
  outcome: OperationalEventOutcome
  revision: string | null
}

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
  maxRetries?: number
  retryDelayMs?: number
  maxRetryDelayMs?: number
}

export interface EventStreamClient {
  close: () => void
  closed: Promise<void>
}

const EVENT_STREAM_PATH = '/api/v1/events/stream'
const DEFAULT_EVENT_STREAM_MAX_RETRIES = 5
const DEFAULT_EVENT_STREAM_RETRY_DELAY_MS = 250
const DEFAULT_EVENT_STREAM_MAX_RETRY_DELAY_MS = 5_000
const OPERATIONAL_EVENT_NAMES: readonly OperationalEventName[] = [
  'generation_prepare',
  'generation_activate',
  'generation_rollback',
  'generation_start',
  'process_shutdown',
  'listener_administrative_state',
  'pool_administrative_state',
  'server_update',
  'unknown',
]
const OPERATIONAL_EVENT_OUTCOMES: readonly OperationalEventOutcome[] = [
  'prepared',
  'rejected',
  'activated',
  'quarantined',
  'requested',
  'applied',
  'unknown',
]

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
  const timestampUnixMs = payload.timestampUnixMs === null
    ? null
    : safeInteger(payload.timestampUnixMs)
      ? payload.timestampUnixMs
      : undefined
  const event = isOperationalEventName(payload.event) ? payload.event : null
  const outcome = isOperationalEventOutcome(payload.outcome) ? payload.outcome : null
  const revision = payload.revision === null
    ? null
    : typeof payload.revision === 'string'
      ? payload.revision
      : undefined
  if (cursor === null || id !== cursor || timestampUnixMs === undefined || event !== eventName ||
    outcome === null || revision === undefined) return null
  return {
    type: 'operational',
    event: { cursor, timestampUnixMs, event, outcome, revision },
  }
}

export function connectEventStream(
  token: string,
  handlers: EventStreamHandlers,
  options: EventStreamOptions = {},
): EventStreamClient {
  const controller = new AbortController()
  let retryTimer: ReturnType<typeof setTimeout> | undefined
  let lastEventId: number | null = null
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
        const response = await fetch(EVENT_STREAM_PATH, {
          cache: 'no-store',
          headers,
          signal: controller.signal,
        })
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

function eventCursor(value: unknown, key: string): number | null {
  if (!isRecord(value)) return null
  return parseEventCursor(value[key])
}

function parseEventCursor(value: unknown): number | null {
  if (safeInteger(value)) return value
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]*)$/.test(value)) return null
  const cursor = Number(value)
  return Number.isSafeInteger(cursor) && cursor >= 0 ? cursor : null
}

function isOperationalEventName(value: unknown): value is OperationalEventName {
  return typeof value === 'string' && OPERATIONAL_EVENT_NAMES.includes(value as OperationalEventName)
}

function isOperationalEventOutcome(value: unknown): value is OperationalEventOutcome {
  return typeof value === 'string' && OPERATIONAL_EVENT_OUTCOMES.includes(value as OperationalEventOutcome)
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

export async function fetchRtmpCatalog(signal?: AbortSignal, token?: string): Promise<RtmpCatalog> {
  return parseRtmpCatalog(await request<unknown>('/api/v1/rtmp/streams', {
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchMonitoring(signal?: AbortSignal, token?: string): Promise<MonitoringSnapshot> {
  return parseMonitoring(await request<unknown>('/api/v1/monitoring', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchTopology(signal?: AbortSignal, token?: string): Promise<TopologySnapshot> {
  return parseTopology(await request<unknown>('/api/v1/topology', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchConfig(token: string, signal?: AbortSignal): Promise<ConfigSnapshot> {
  return parseConfigSnapshot(await request<unknown>('/api/v1/config', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200))
}

export async function validateConfig(
  config: CanonicalConfig,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigValidationResponse> {
  return parseValidation(await request<unknown>('/api/v1/config/validate', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ config } satisfies ConfigRequest),
    signal,
  }, 200))
}

export async function saveConfig(
  config: CanonicalConfig,
  diskRevision: string,
  token: string,
  signal?: AbortSignal,
): Promise<ConfigSaveResponse> {
  return parseSave(await request<unknown>('/api/v1/config', {
    method: 'PUT',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      'If-Config-Revision': diskRevision,
    },
    body: JSON.stringify({ config } satisfies ConfigRequest),
    signal,
  }, 200))
}

export async function setRecording(
  streamId: string,
  recorderId: string,
  action: 'start' | 'stop',
  token?: string,
): Promise<RecorderSnapshot> {
  const headers = token ? {
    ...authorizationHeader(token),
    'If-Generation-Revision': await fetchActiveRevision(token),
  } : undefined
  return parseRecorder(await request<unknown>(
    `/api/v1/rtmp/streams/${streamId}/recorders/${recorderId}/${action}`,
    { method: 'POST', headers },
  ))
}

async function fetchActiveRevision(token: string): Promise<string> {
  const value = await request<unknown>('/api/v1/generations', {
    cache: 'no-store',
    headers: authorizationHeader(token),
  })
  if (!isRecord(value) || !isRecord(value.generation) || typeof value.generation.activeRevision !== 'string') {
    throw new Error('generation response has no active revision')
  }
  return value.generation.activeRevision
}

export class ApiError extends Error {
  readonly status: number
  readonly payload: unknown
  readonly code: string | null

  constructor(status: number, message: string, payload: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.payload = payload
    this.code = apiErrorCode(payload)
  }
}

function authorizationHeader(token: string): Record<string, string> {
  return { Authorization: `Bearer ${token}` }
}

async function request<T>(url: string, init?: RequestInit, expectedStatus?: number): Promise<T> {
  const response = await fetch(url, init)
  const payload = await response.json() as unknown
  if (!response.ok || (expectedStatus !== undefined && response.status !== expectedStatus)) {
    throw new ApiError(
      response.status,
      apiErrorMessage(payload) ?? `Request returned unexpected status ${response.status}`,
      payload,
    )
  }
  return payload as T
}

function apiErrorMessage(value: unknown): string | null {
  return isRecord(value) && isRecord(value.error) && typeof value.error.message === 'string'
    ? value.error.message
    : null
}

function apiErrorCode(value: unknown): string | null {
  return isRecord(value) && isRecord(value.error) && typeof value.error.code === 'string'
    ? value.error.code
    : null
}

function parseRtmpCatalog(value: unknown): RtmpCatalog {
  if (!isRecord(value) || !decimalString(value.revision) || !safeInteger(value.as_of_unix_ms) ||
    !isRecord(value.capabilities) || typeof value.capabilities.live_ingest !== 'boolean' ||
    typeof value.capabilities.manual_recording !== 'boolean' || !Array.isArray(value.streams) ||
    !value.streams.every(isStream)
  ) return invalidPayload('RTMP catalog')
  const manualRecording = value.capabilities.manual_recording
  if (value.streams.some((stream) =>
    stream.recording_supported !== (stream.recorders.length > 0) ||
    stream.manual_recording !== stream.recorders.some((recorder) => recorder.manual) ||
    (stream.manual_recording && !manualRecording)
  )) return invalidPayload('RTMP catalog')
  return value as unknown as RtmpCatalog
}

function parseMonitoring(value: unknown): MonitoringSnapshot {
  if (!isRecord(value) || !safeInteger(value.sampledAtUnixMs) || !safeInteger(value.uptimeMs) ||
    !monitoringProcess(value.process) || !monitoringHost(value.host) || !monitoringTraffic(value.traffic) ||
    !Array.isArray(value.listeners) || !value.listeners.every(monitoringListener) ||
    !Array.isArray(value.upstreamPools) || !value.upstreamPools.every(monitoringPool) ||
    !Array.isArray(value.certbotCertificates) || !value.certbotCertificates.every(certbotCertificate) ||
    !(value.certbotWatcher === null || certbotWatcher(value.certbotWatcher)) || !isRecord(value.rtmp) ||
    !safeInteger(value.rtmp.activeStreams) || !safeInteger(value.rtmp.publishers) ||
    !safeInteger(value.rtmp.subscribers) || !decimalString(value.rtmp.mediaPayloadBytesReceived) ||
    typeof value.rtmp.recordingSupported !== 'boolean' || typeof value.rtmp.manualRecording !== 'boolean' ||
    !decimalString(value.rtmp.recorderBytesWritten) || !decimalString(value.rtmp.recorderSegmentsStarted) ||
    !decimalString(value.rtmp.recorderSegmentsCompleted) || !decimalString(value.rtmp.recorderDiscontinuities) ||
    !decimalString(value.rtmp.relayConnectionAttempts) || !decimalString(value.rtmp.relayConnections) ||
    !decimalString(value.rtmp.relayReconnects) || !decimalString(value.rtmp.relayEventsSent) ||
    !decimalString(value.rtmp.relayEventsDropped) || !decimalString(value.rtmp.relayPayloadBytesSent) ||
    !Array.isArray(value.rtmp.relays) || !value.rtmp.relays.every(monitoringRelay) ||
    !Array.isArray(value.rtmp.recorders) || !value.rtmp.recorders.every(monitoringRecorder)
  ) return invalidPayload('monitoring')
  return value as unknown as MonitoringSnapshot
}

function parseConfigSnapshot(value: unknown): ConfigSnapshot {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !isCanonicalConfig(value.config) || !configurationSource(value) || !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration')
  return {
    schemaVersion: 1,
    diskRevision: value.diskRevision,
    candidateRevision: value.candidateRevision,
    activeRevision: value.activeRevision,
    config: value.config,
    configFormat: value.configFormat,
    compositional: value.compositional,
    dependencyCount: value.dependencyCount,
    configPreview: value.configPreview,
    ...(value.luaPreview === undefined ? {} : { luaPreview: value.luaPreview }),
    diagnostics: value.diagnostics,
  }
}

function parseValidation(value: unknown): ConfigValidationResponse {
  if (!isRecord(value) || typeof value.candidateRevision !== 'string' ||
    !isCanonicalConfig(value.normalizedConfig) || !configurationSource(value) ||
    !diagnostics(value.diagnostics) || typeof value.restartRequired !== 'boolean' ||
    !candidateTopology(value.topology)
  ) return invalidPayload('configuration validation')
  return {
    candidateRevision: value.candidateRevision,
    normalizedConfig: value.normalizedConfig,
    configFormat: value.configFormat,
    compositional: value.compositional,
    dependencyCount: value.dependencyCount,
    configPreview: value.configPreview,
    ...(value.luaPreview === undefined ? {} : { luaPreview: value.luaPreview }),
    diagnostics: value.diagnostics,
    restartRequired: value.restartRequired,
    topology: value.topology,
  }
}

function parseSave(value: unknown): ConfigSaveResponse {
  if (!isRecord(value) || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration save')
  const pending = value.outcome === 'saved_pending_activation' &&
    value.activationState === 'pending' && value.restartRequired === false
  const unchanged = value.outcome === 'unchanged_active' &&
    value.activationState === 'active' && value.restartRequired === false
  const restart = value.outcome === 'saved_restart_required' &&
    value.activationState === 'restart_required' && value.restartRequired === true
  if (!pending && !unchanged && !restart) return invalidPayload('configuration save')
  return value as unknown as ConfigSaveResponse
}

function configurationSource(value: Record<string, unknown>): value is Record<string, unknown> & {
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
} {
  return ['kdl', 'lua', 'uci', 'hocon'].includes(String(value.configFormat)) &&
    typeof value.compositional === 'boolean' && safeInteger(value.dependencyCount) &&
    typeof value.configPreview === 'string' &&
    (value.luaPreview === undefined || typeof value.luaPreview === 'string')
}

function parseTopology(value: unknown): TopologySnapshot {
  if (!isRecord(value) || value.schemaVersion !== 1 || !isRecord(value.state) ||
    value.state.config !== 'active' || !['active', 'starting', 'degraded'].includes(String(value.state.runtime)) ||
    !safeInteger(value.state.sampledAtUnixMs) || !Array.isArray(value.nodes) ||
    !value.nodes.every(topologyNode) || !Array.isArray(value.edges) ||
    !value.edges.every(topologyEdge) || !Array.isArray(value.overlays) ||
    !value.overlays.every(topologyOverlay)
  ) return invalidPayload('topology')
  return value as unknown as TopologySnapshot
}

function parseRecorder(value: unknown): RecorderSnapshot {
  if (!isRecorder(value)) return invalidPayload('recorder command')
  return value
}

function isStream(value: unknown): value is StreamSnapshot {
  return isRecord(value) && typeof value.id === 'string' && decimalString(value.revision) &&
    typeof value.server_id === 'string' && typeof value.application === 'string' &&
    typeof value.name === 'string' && safeInteger(value.created_at_unix_ms) &&
    (value.publisher === null || (isRecord(value.publisher) &&
      typeof value.publisher.session_id === 'string' && safeInteger(value.publisher.attached_at_unix_ms))) &&
    safeInteger(value.subscriber_count) && isRecord(value.media) && isTrack(value.media.audio) &&
    isTrack(value.media.video) && decimalString(value.media.fanout_payload_bytes) &&
    Array.isArray(value.relays) && value.relays.every(isRelay) &&
    typeof value.recording_supported === 'boolean' && typeof value.manual_recording === 'boolean' &&
    Array.isArray(value.recorders) && value.recorders.every(isRecorder)
}

function isRelay(value: unknown): value is RelaySnapshot {
  return isRecord(value) && typeof value.id === 'string' && isRecord(value.destination) &&
    typeof value.destination.address === 'string' && typeof value.destination.application === 'string' &&
    typeof value.destination.stream_name === 'string' &&
    ['connecting', 'publishing', 'backoff', 'stopped'].includes(String(value.phase)) &&
    (value.last_failure === null || ['connect', 'handshake', 'session', 'transport', 'thread']
      .includes(String(value.last_failure))) && safeInteger(value.queue_messages) &&
    ['queue_bytes', 'connection_attempts', 'connections', 'reconnects', 'events_enqueued',
      'events_sent', 'events_dropped', 'payload_bytes_sent'].every((key) => decimalString(value[key]))
}

function isTrack(value: unknown): value is TrackSnapshot {
  return isRecord(value) && (value.codec_id === null || safeInteger(value.codec_id)) &&
    (value.codec_fourcc === null || typeof value.codec_fourcc === 'string') &&
    (value.codec_name === null || typeof value.codec_name === 'string') &&
    typeof value.recording_supported === 'boolean' && decimalString(value.payload_bytes) &&
    (value.last_rtmp_timestamp_ms === null || safeInteger(value.last_rtmp_timestamp_ms)) &&
    (value.last_observed_at_unix_ms === null || safeInteger(value.last_observed_at_unix_ms))
}

function isRecorder(value: unknown): value is RecorderSnapshot {
  return isRecord(value) && typeof value.id === 'string' &&
    (value.name === null || typeof value.name === 'string') && typeof value.manual === 'boolean' &&
    recorderPhase(value.phase) &&
    safeInteger(value.changed_at_unix_ms) && decimalString(value.bytes_written) &&
    decimalString(value.segments_started) && decimalString(value.segments_completed) &&
    decimalString(value.discontinuities) && nullableString(value.current_relative_name) &&
    nullableString(value.published_but_not_durable_relative_name) &&
    nullableString(value.last_completed_relative_name) && nullableString(value.recoverable_partial_name)
}

function recorderPhase(value: unknown): boolean {
  if (!isRecord(value) || !['idle', 'starting', 'recording', 'stopping', 'failed'].includes(String(value.state))) {
    return false
  }
  if (value.state === 'idle') return true
  if (typeof value.operation_id !== 'string') return false
  if (value.state === 'recording') return safeInteger(value.started_at_unix_ms)
  if (value.state === 'failed') return typeof value.code === 'string'
  return true
}

function monitoringProcess(value: unknown): boolean {
  return isRecord(value) && safeInteger(value.activeConnections) &&
    ['ready', 'drain', 'maintenance'].includes(String(value.administrativeState)) &&
    (value.cpuPercent === null || finiteNumber(value.cpuPercent)) &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    decimalString(value.rejectedConnections) && decimalString(value.retryAttempts) &&
    safeInteger(value.residentMemoryBytes) && safeInteger(value.virtualMemoryBytes) &&
    safeInteger(value.threadCount) && safeInteger(value.openFileDescriptors)
}

function monitoringHost(value: unknown): boolean {
  return isRecord(value) && finiteNumber(value.loadAverage1m) && finiteNumber(value.loadAverage5m) &&
    finiteNumber(value.loadAverage15m) && safeInteger(value.totalMemoryBytes) &&
    safeInteger(value.availableMemoryBytes)
}

function monitoringTraffic(value: unknown): boolean {
  return isRecord(value) && decimalString(value.acceptedConnections) &&
    decimalString(value.rejectedConnections) && safeInteger(value.activeConnections) &&
    decimalString(value.bytesReceived) && decimalString(value.bytesSent)
}

function monitoringListener(value: unknown): boolean {
  return monitoringTraffic(value) && isRecord(value) && typeof value.name === 'string' &&
    ['http', 'tcp', 'rtmp', 'forward_http1', 'forward_http2', 'forward_http3']
      .includes(String(value.protocol)) && typeof value.bind === 'string' &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    ['configured', 'listening', 'stopped', 'failed'].includes(String(value.state))
}

function monitoringPool(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' &&
    ['first', 'round_robin', 'least_connections'].includes(String(value.algorithm)) &&
    safeInteger(value.availableEndpoints) && safeInteger(value.totalEndpoints) &&
    decimalString(value.unavailableSelections) && safeInteger(value.queued) &&
    decimalString(value.queuedTotal) && decimalString(value.queueTimeouts) &&
    decimalString(value.queueCancellations) && Array.isArray(value.endpoints) &&
    value.endpoints.every((endpoint) => isRecord(endpoint) && typeof endpoint.address === 'string' &&
      typeof endpoint.name === 'string' && decimalString(endpoint.activeConnections) &&
      ['ready', 'drain', 'maintenance'].includes(String(endpoint.administrativeState)) &&
      typeof endpoint.checksEnabled === 'boolean' && typeof endpoint.checksRunning === 'boolean' &&
      (endpoint.configuredMaxConnections === null || safeInteger(endpoint.configuredMaxConnections)) &&
      ['auto', 'up', 'down'].includes(String(endpoint.healthOverride)) &&
      (endpoint.maxConnections === null || safeInteger(endpoint.maxConnections)) &&
      ['unchecked', 'unknown', 'healthy', 'unhealthy'].includes(String(endpoint.state)) &&
      nullableSafeInteger(endpoint.lastCheckedAtUnixMs) && nullableSafeInteger(endpoint.lastTransitionAtUnixMs) &&
      decimalString(endpoint.successfulChecks) && decimalString(endpoint.failedChecks) &&
      decimalString(endpoint.consecutiveSuccesses) && decimalString(endpoint.consecutiveFailures) &&
      (endpoint.lastFailure === null || ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error']
        .includes(String(endpoint.lastFailure))))
}

function certbotCertificate(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' && safeInteger(value.activeArchiveRevision) &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function certbotWatcher(value: unknown): boolean {
  return isRecord(value) && ['healthy', 'degraded', 'stopped'].includes(String(value.health)) &&
    ['coalescedEvents', 'ignoredAccessEvents', 'backendErrors', 'watchRecoveries', 'watchRefreshes',
      'rescans', 'periodicRescans', 'reconciliationFailures'].every((key) => decimalString(value[key]))
}

function monitoringRecorder(value: unknown): boolean {
  return isRecord(value) && typeof value.streamId === 'string' && typeof value.recorderId === 'string' &&
    nullableString(value.name) && typeof value.manual === 'boolean' &&
    ['idle', 'starting', 'recording', 'stopping', 'failed'].includes(String(value.phase)) &&
    decimalString(value.bytesWritten) && decimalString(value.segmentsStarted) &&
    decimalString(value.segmentsCompleted) && decimalString(value.discontinuities) &&
    nullableString(value.currentRelativeName) && nullableString(value.lastCompletedRelativeName) &&
    nullableString(value.recoverablePartialName) && nullableString(value.publishedButNotDurableRelativeName)
}

function monitoringRelay(value: unknown): boolean {
  return isRecord(value) && typeof value.streamId === 'string' && typeof value.relayId === 'string' &&
    typeof value.address === 'string' && typeof value.application === 'string' &&
    typeof value.streamName === 'string' &&
    ['connecting', 'publishing', 'backoff', 'stopped'].includes(String(value.phase)) &&
    (value.lastFailure === null || ['connect', 'handshake', 'session', 'transport', 'thread']
      .includes(String(value.lastFailure))) && safeInteger(value.queueMessages) &&
    ['queueBytes', 'connectionAttempts', 'connections', 'reconnects', 'eventsSent', 'eventsDropped',
      'payloadBytesSent'].every((key) => decimalString(value[key]))
}

function topologyNode(value: unknown): boolean {
  return isRecord(value) && typeof value.id === 'string' && typeof value.name === 'string' &&
    typeof value.configPath === 'string' && isRecord(value.attributes) &&
    ['listener', 'forward_proxy_listener', 'forward_proxy_service', 'rtmp_listener', 'tls_profile', 'certificate',
      'http_service', 'http_route', 'l4_service', 'upstream_pool', 'endpoint']
      .includes(String(value.kind))
}

function topologyEdge(value: unknown): boolean {
  return isRecord(value) && typeof value.id === 'string' && typeof value.source === 'string' &&
    typeof value.target === 'string' && typeof value.configPath === 'string' &&
    ['dispatch_service', 'service_route', 'route_pool', 'service_pool', 'pool_endpoint',
      'listener_tls', 'tls_certificate'].includes(String(value.kind))
}

function topologyOverlay(value: unknown): boolean {
  return isRecord(value) && typeof value.nodeId === 'string' && topologyMetrics(value.metrics) &&
    ['configured', 'listening', 'stopped', 'failed', 'available', 'degraded', 'unavailable',
      'unchecked', 'unknown', 'healthy', 'unhealthy']
      .includes(String(value.state))
}

function topologyMetrics(value: unknown): boolean {
  if (!isRecord(value)) return false
  const decimalKeys = [
    'acceptedConnections', 'rejectedConnections', 'bytesReceived', 'bytesSent',
    'unavailableSelections', 'queuedTotal', 'queueTimeouts', 'queueCancellations',
    'successfulChecks', 'failedChecks', 'consecutiveSuccesses', 'consecutiveFailures',
  ]
  const numberKeys = ['availableEndpoints', 'totalEndpoints', 'queued']
  const activeConnections = value.activeConnections === undefined ||
    decimalString(value.activeConnections) || safeInteger(value.activeConnections)
  return activeConnections && decimalKeys.every((key) => value[key] === undefined || decimalString(value[key])) &&
    numberKeys.every((key) => value[key] === undefined || safeInteger(value[key])) &&
    (value.maxConnections === undefined || value.maxConnections === null || safeInteger(value.maxConnections)) &&
    ['lastCheckedAtUnixMs', 'lastTransitionAtUnixMs']
      .every((key) => value[key] === undefined || nullableSafeInteger(value[key])) &&
    (value.lastFailure === undefined || value.lastFailure === null ||
      ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'].includes(String(value.lastFailure)))
}

function candidateTopology(value: unknown): value is CandidateTopologySnapshot {
  return isRecord(value) && value.schemaVersion === 1 && isRecord(value.state) &&
    value.state.config === 'candidate' && value.state.runtime === 'not_active' &&
    safeInteger(value.state.sampledAtUnixMs) && Array.isArray(value.nodes) &&
    value.nodes.every(topologyNode) && Array.isArray(value.edges) && value.edges.every(topologyEdge) &&
    Array.isArray(value.overlays) && value.overlays.length === 0
}

function diagnostics(value: unknown): value is ConfigDiagnostic[] {
  return Array.isArray(value) && value.every(isConfigDiagnostic)
}

function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
