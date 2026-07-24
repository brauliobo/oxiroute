import type {
  CanonicalConfig,
  ConfigDiagnostic,
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
  cpuPercent: number | null
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

export interface MonitoringListener extends MonitoringTraffic {
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
  recorders: MonitoringRecorder[]
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

export interface MonitoringPoolEndpoint {
  address: string
  activeLeases: string
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
  activeConnections?: number
  acceptedConnections?: string
  rejectedConnections?: string
  bytesReceived?: string
  bytesSent?: string
  availableEndpoints?: number
  totalEndpoints?: number
  unavailableSelections?: string
  activeLeases?: string
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

export async function fetchRtmpCatalog(signal?: AbortSignal): Promise<RtmpCatalog> {
  return parseRtmpCatalog(await request<unknown>('/api/v1/rtmp/streams', { signal }))
}

export async function fetchMonitoring(signal?: AbortSignal): Promise<MonitoringSnapshot> {
  return parseMonitoring(await request<unknown>('/api/v1/monitoring', { cache: 'no-store', signal }))
}

export async function fetchTopology(signal?: AbortSignal): Promise<TopologySnapshot> {
  return parseTopology(await request<unknown>('/api/v1/topology', { cache: 'no-store', signal }))
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
): Promise<RecorderSnapshot> {
  return parseRecorder(await request<unknown>(
    `/api/v1/rtmp/streams/${streamId}/recorders/${recorderId}/${action}`,
    { method: 'POST' },
  ))
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
    !Array.isArray(value.rtmp.recorders) || !value.rtmp.recorders.every(monitoringRecorder)
  ) return invalidPayload('monitoring')
  return value as unknown as MonitoringSnapshot
}

function parseConfigSnapshot(value: unknown): ConfigSnapshot {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.diskRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !isCanonicalConfig(value.config) || !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration')
  return value as unknown as ConfigSnapshot
}

function parseValidation(value: unknown): ConfigValidationResponse {
  if (!isRecord(value) || typeof value.candidateRevision !== 'string' ||
    !isCanonicalConfig(value.normalizedConfig) || typeof value.luaPreview !== 'string' ||
    !diagnostics(value.diagnostics) || !candidateTopology(value.topology)
  ) return invalidPayload('configuration validation')
  return value as unknown as ConfigValidationResponse
}

function parseSave(value: unknown): ConfigSaveResponse {
  if (!isRecord(value) || typeof value.diskRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !diagnostics(value.diagnostics)
  ) return invalidPayload('configuration save')
  const restart = value.outcome === 'saved_restart_required' &&
    value.activationState === 'restart_required' && value.restartRequired === true
  const unchanged = value.outcome === 'unchanged_active' &&
    value.activationState === 'active' && value.restartRequired === false
  if (!restart && !unchanged) return invalidPayload('configuration save')
  return value as unknown as ConfigSaveResponse
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
    typeof value.recording_supported === 'boolean' && typeof value.manual_recording === 'boolean' &&
    Array.isArray(value.recorders) && value.recorders.every(isRecorder)
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
  return isRecord(value) && (value.cpuPercent === null || finiteNumber(value.cpuPercent)) &&
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
    ['round_robin', 'least_connections'].includes(String(value.algorithm)) &&
    safeInteger(value.availableEndpoints) && safeInteger(value.totalEndpoints) &&
    decimalString(value.unavailableSelections) && Array.isArray(value.endpoints) &&
    value.endpoints.every((endpoint) => isRecord(endpoint) && typeof endpoint.address === 'string' &&
      decimalString(endpoint.activeLeases) && ['unchecked', 'unknown', 'healthy', 'unhealthy'].includes(String(endpoint.state)) &&
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

function topologyNode(value: unknown): boolean {
  return isRecord(value) && typeof value.id === 'string' && typeof value.name === 'string' &&
    typeof value.configPath === 'string' && isRecord(value.attributes) &&
    ['listener', 'forward_proxy_listener', 'rtmp_listener', 'tls_profile', 'certificate',
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
    'unavailableSelections', 'activeLeases', 'successfulChecks', 'failedChecks',
    'consecutiveSuccesses', 'consecutiveFailures',
  ]
  const numberKeys = ['activeConnections', 'availableEndpoints', 'totalEndpoints']
  return decimalKeys.every((key) => value[key] === undefined || decimalString(value[key])) &&
    numberKeys.every((key) => value[key] === undefined || safeInteger(value[key])) &&
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
