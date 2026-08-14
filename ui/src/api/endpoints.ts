import type {
  CanonicalConfig,
  ConfigDiagnostic,
  ConfigFormat,
  ConfigRequest,
  ConfigSaveResponse,
  ConfigSnapshot,
  HttpRetryTrigger,
  ListenerBind,
  UpstreamEndpoint,
} from '../config'
import { isCanonicalConfig, isConfigDiagnostic } from '../config'
import {
  decimalString,
  finiteNumber,
  isRecord,
  nullableSafeInteger,
  nullableString,
  safeInteger,
} from '../valueGuards'
import {
  monitoringListener,
  monitoringPool,
  monitoringPoolEndpoint,
  parseListenerInventory,
  parsePoolInventory,
  parseRuntimeStatus,
  parseServerInventory,
} from './decoders'
import { ApiError, apiErrorMessage, request } from './transport'
import type {
  AdministrativeState,
  EndpointHealthState,
  HealthFailure,
  HealthOverride,
  ListenerInventoryResponse,
  ListenerRuntimeState,
  MonitoringCache,
  MonitoringHttpOperations,
  MonitoringListener,
  MonitoringListenerProtocol,
  MonitoringPool,
  MonitoringPoolEndpoint,
  MonitoringProxyProtocol,
  MonitoringTcpRelays,
  MonitoringTraffic,
  MonitoringUpstreamAlgorithm,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryEntry,
  ServerInventoryResponse,
  ServerTarget,
} from './managementContracts'

export type {
  AdministrativeState,
  EndpointHealthState,
  HealthFailure,
  HealthOverride,
  ListenerInventoryResponse,
  ListenerRuntimeState,
  MonitoringCache,
  MonitoringHttpOperations,
  MonitoringListener,
  MonitoringListenerProtocol,
  MonitoringPool,
  MonitoringPoolEndpoint,
  MonitoringProxyProtocol,
  MonitoringTcpRelays,
  MonitoringTraffic,
  MonitoringUpstreamAlgorithm,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryEntry,
  ServerInventoryResponse,
  ServerTarget,
}

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

export type RecorderErrorCode =
  | 'open_failed'
  | 'write_failed'
  | 'close_failed'
  | 'backend_unavailable'
  | 'file_sync_failed'
  | 'publish_failed'
  | 'directory_sync_failed'
  | 'queue_discontinuity'
  | 'unsupported_codec'
  | 'shutdown_timed_out'
  | 'worker_panicked'
  | 'stale_publisher'
export type RecorderNotification = 'started' | 'stopped' | 'failed'
export type RecorderPhase =
  | { state: 'idle' }
  | { state: 'starting'; operation_id: string }
  | { state: 'recording'; operation_id: string; started_at_unix_ms: number }
  | { state: 'stopping'; operation_id: string }
  | { state: 'failed'; operation_id: string; code: RecorderErrorCode }

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
  last_notification: RecorderNotification | null
}

export type RtmpRelayPhase = 'connecting' | 'publishing' | 'pulling' | 'backoff' | 'stopped'
export type RtmpRelayFailure = 'policy' | 'connect' | 'handshake' | 'session' | 'transport' | 'source' | 'thread'
export type RtmpRelayDnsRefreshFailure = 'resolution' | 'address_set' | 'policy' | 'direct_loop' | 'family_mismatch'

const RTMP_RELAY_PHASES: readonly RtmpRelayPhase[] = ['connecting', 'publishing', 'pulling', 'backoff', 'stopped']
const RTMP_RELAY_FAILURES: readonly RtmpRelayFailure[] = ['policy', 'connect', 'handshake', 'session', 'transport', 'source', 'thread']
const RTMP_RELAY_DNS_REFRESH_FAILURES: readonly RtmpRelayDnsRefreshFailure[] = [
  'resolution', 'address_set', 'policy', 'direct_loop', 'family_mismatch',
]

export interface RelaySnapshot {
  id: string
  destination: { address: string; application: string; stream_name: string }
  phase: RtmpRelayPhase
  last_failure: RtmpRelayFailure | null
  queue_messages: number
  queue_bytes: string
  connection_attempts: string
  connections: string
  reconnects: string
  dns_refresh_attempts: string
  dns_refresh_successes: string
  dns_refresh_failures: string
  last_dns_refresh_failure: RtmpRelayDnsRefreshFailure | null
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

export type RtmpClientRole = 'client' | 'publisher' | 'subscriber'
export type RtmpClientControlTarget = RtmpClientRole

export interface RtmpClientSnapshot {
  id: string
  service: string
  peerIp: string | null
  connectedAtUnixMs: number
  application: string | null
  stream: string | null
  role: RtmpClientRole
  revision: string
}

export interface RtmpStatsGlobal {
  activeStreams: number
  publishers: number
  subscribers: number
  audioPayloadBytes: string
  videoPayloadBytes: string
  liveIngest: boolean
  manualRecording: boolean
}

export interface RtmpLiveStat {
  id: string
  service: string
  application: string
  name: string
  createdAtUnixMs: number
  publisherSessionId: string | null
  subscriberCount: number
  audioPayloadBytes: string
  videoPayloadBytes: string
}

export interface RtmpStats {
  revision: string
  asOfUnixMs: number
  global: RtmpStatsGlobal
  live: RtmpLiveStat[]
  clients: RtmpClientSnapshot[]
  liveTruncated: boolean
  clientsTruncated: boolean
}

export interface RtmpControlResponse {
  outcome: 'requested' | 'already_requested'
  sessionId: string
  target: RtmpClientControlTarget
  sessionRevision: string
}

export interface MonitoringProcess {
  activeConnections: number
  administrativeState: AdministrativeState
  status: MonitoringComponentStatus
  cpuPercent: number | null
  maxConnections: number | null
  rejectedConnections: string
  retryAttempts: string
  residentMemoryBytes: number | null
  virtualMemoryBytes: number | null
  threadCount: number | null
  openFileDescriptors: number | null
}

export interface MonitoringHost {
  status: MonitoringComponentStatus
  loadAverage1m: number | null
  loadAverage5m: number | null
  loadAverage15m: number | null
  totalMemoryBytes: number | null
  availableMemoryBytes: number | null
}

export type MonitoringComponentState = 'healthy' | 'degraded' | 'unsupported'

export interface MonitoringComponentStatus {
  state: MonitoringComponentState
  reason?: string
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
  relayDnsRefreshAttempts: string
  relayDnsRefreshSuccesses: string
  relayDnsRefreshFailures: string
  relayEventsSent: string
  relayEventsDropped: string
  relayPayloadBytesSent: string
  accessLog: MonitoringRtmpAccessLog
  relays: MonitoringRelay[]
  recorders: MonitoringRecorder[]
}

export interface MonitoringRtmpAccessLog {
  queueCapacity: number
  queueDepth: string
  enqueued: string
  written: string
  dropped: string
  queueSaturated: string
  writeFailures: string
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
  dnsRefreshAttempts: string
  dnsRefreshSuccesses: string
  dnsRefreshFailures: string
  lastDnsRefreshFailure: RtmpRelayDnsRefreshFailure | null
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

export interface DirectFileCertificateSnapshot {
  name: string
  activeContentRevision: string
  expiresAt: string
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface AcmeManagedCertificateSnapshot {
  name: string
  directoryUrl: string
  diskRevision: string
  activeRevision: string
  expiresAt: string
  notBeforeUnixSeconds: number | null
  notAfterUnixSeconds: number | null
  nextActionUnixSeconds: number | null
  lastOutcome: string | null
  lastErrorCode: string | null
  renewalInformationStatus: string
  dnsProvider: string | null
  dnsProviderDeployment: string | null
  dnsProviderHealth: string | null
  dnsCleanupStatus: string
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

export type DirectFileWatcherSnapshot = CertbotWatcherSnapshot

export type MonitoringTransport = 'http' | 'rtmp' | 'forward' | 'cache' | 'tcp' | 'udp' | 'h3' | 'acme'
export type MonitoringTransportOutcome =
  | 'success'
  | 'client_error'
  | 'server_error'
  | 'upstream_error'
  | 'timeout'
  | 'rejected'
  | 'cancelled'
  | 'internal_error'
  | 'degraded'

export interface MonitoringTransportOperation {
  transport: MonitoringTransport
  outcomes: Array<{ outcome: MonitoringTransportOutcome; count: string }>
  latency: {
    buckets: Array<{ upperBoundMs: number | null; count: string }>
    count: string
    sumMs: string
  }
}

export interface MonitoringAccessRecord {
  timestampUnixMs: number
  correlationId: string
  listener: string
  transport: MonitoringTransport
  outcome: MonitoringTransportOutcome
  durationMs: string
  bytesReceived: string
  bytesSent: string
}

export interface MonitoringSnapshot {
  sampledAtUnixMs: number
  uptimeMs: number
  generationAgeMs: number
  process: MonitoringProcess
  host: MonitoringHost
  traffic: MonitoringTraffic
  listeners: MonitoringListener[]
  upstreamPools: MonitoringPool[]
  transportOperations: MonitoringTransportOperation[]
  accessRecords: MonitoringAccessRecord[]
  certbotCertificates: CertbotCertificateSnapshot[]
  certbotWatcher: CertbotWatcherSnapshot | null
  acmeManagedCertificates: AcmeManagedCertificateSnapshot[]
  directFileCertificates: DirectFileCertificateSnapshot[]
  directFileWatcher: DirectFileWatcherSnapshot | null
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

export interface ConfigValidationResponse {
  candidateRevision: string
  normalizedConfig: CanonicalConfig
  configFormat: ConfigFormat
  compositional: boolean
  dependencyCount: number
  configPreview: string
  luaPreview?: string
  diagnostics: ConfigDiagnostic[]
  restartRequired: boolean
  topology: CandidateTopologySnapshot
}

export interface OperationalEventSimpleOutcomes {
  generation_prepare: 'prepared' | 'rejected' | 'requested' | 'failed'
  generation_activate: 'activated'
  generation_rollback: 'prepared' | 'rejected' | 'requested' | 'failed'
  generation_drain: 'rejected' | 'requested' | 'failed'
  generation_start: 'quarantined'
  configuration_reload: 'rejected' | 'applied' | 'failed'
  import_completed: 'applied'
  control_operation: 'rejected' | 'requested' | 'failed'
  process_shutdown: 'rejected' | 'requested' | 'failed'
  listener_administrative_state: 'rejected' | 'applied' | 'failed'
  pool_administrative_state: 'rejected' | 'applied' | 'failed'
  server_update: 'rejected' | 'applied' | 'failed'
  rtmp_connect: 'rejected' | 'applied' | 'failed'
  rtmp_publish: 'rejected' | 'applied' | 'failed'
  rtmp_play: 'rejected' | 'applied' | 'failed'
  rtmp_disconnect: 'rejected' | 'applied' | 'failed'
  rtmp_access: 'rejected' | 'applied' | 'failed'
  certificate_renewal: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_activated: 'activated'
  certificate_revocation: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_deletion: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_account_rollover: 'rejected' | 'requested' | 'applied' | 'failed'
  certificate_job_control: 'rejected' | 'requested' | 'applied' | 'failed'
  unknown: 'unknown'
}

export type OperationalEventSimpleName = keyof OperationalEventSimpleOutcomes

export type OperationalEventName =
  | OperationalEventSimpleName
  | 'upstream_endpoint_ejection'
  | 'upstream_endpoint_recovery'

export type OperationalEventSimpleOutcome = OperationalEventSimpleOutcomes[OperationalEventSimpleName]

export interface OperationalEndpointEjectionOutcome {
  type: 'ejected'
  pool: string
  server: string
  reason: HealthFailure
  failureCount: number
  ejectionCount: number
  ejectedAtUnixMs: number
  ejectionUntilUnixMs: number
}

export interface OperationalEndpointRecoveryOutcome {
  type: 'recovered'
  pool: string
  server: string
  reason: HealthFailure | null
  recoveryCount: number
  recoveredAtUnixMs: number
}

export type OperationalEventOutcome =
  | OperationalEventSimpleOutcome
  | OperationalEndpointEjectionOutcome
  | OperationalEndpointRecoveryOutcome

interface OperationalEventBase {
  cursor: number
  timestampUnixMs: number | null
  revision: string | null
  certificate?: string
}

export type OperationalEvent =
  | OperationalEventBase & {
    event: 'upstream_endpoint_ejection'
    outcome: OperationalEndpointEjectionOutcome
  }
  | OperationalEventBase & {
    event: 'upstream_endpoint_recovery'
    outcome: OperationalEndpointRecoveryOutcome
  }
  | {
    [Event in OperationalEventSimpleName]: OperationalEventBase & {
      event: Event
      outcome: OperationalEventSimpleOutcomes[Event]
    }
  }[OperationalEventSimpleName]

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

export interface GenerationStatus {
  buildVersion: string
  diskRevision: string | null
  candidateRevision: string | null
  activeRevision: string | null
  previousRevision: string | null
  quarantinedRevision: string | null
  activeAccepting: boolean
  degraded: boolean
  lastFailure: string | null
  prepares: number
  activations: number
  failures: number
  rollbacks: number
}

export interface GenerationResponse {
  generation: GenerationStatus
}

export interface MutationResponse {
  outcome: string
  changed?: number
}

export interface DnsRefreshResult {
  pool: string
  server: string
  outcome: 'refreshed' | 'failed'
  addresses?: string[]
  errorCode?: string
}

export interface DnsRefreshResponse {
  outcome: 'refreshed' | 'partially_refreshed'
  atomic: false
  servers: DnsRefreshResult[]
}

export interface EventPage {
  events: OperationalEvent[]
  cursor: number
  latestCursor: number
  hasMore: boolean
  oldestCursor: number | null
}

export type AuditCategory = 'reload' | 'import' | 'certificate' | 'control'
export type AuditResult = 'requested' | 'succeeded' | 'failed' | 'rejected' | 'conflict' | 'partial' | 'degraded'

export interface AuditRecord {
  id: number
  timestampUnixMs: number
  correlationId: string
  actor: string
  source: string
  category: AuditCategory
  operation: string
  result: AuditResult
  revision?: string
}

export interface AuditPage {
  records: AuditRecord[]
  cursor: number
  latestCursor: number
  hasMore: boolean
  oldestCursor: number | null
}

export type AuditComponentState = 'healthy' | 'degraded' | 'memory'

export interface AuditStatus {
  state: AuditComponentState
  persistent: boolean
  degraded: boolean
  recordCount: number
  bytes: number
  rotatedFiles: number
  maxRecords: number
  maxRecordBytes: number
  maxFileBytes: number
  maxTotalBytes: number
  maxRotatedFiles: number
  writeFailures: number
  corruptRecords: number
  lastError?: string
}

export interface AuditStatusResponse {
  audit: AuditStatus
}

export interface TlsMaterialStatus {
  activeContentRevision: string
  expiresAt: string
  activeArchiveRevision?: number
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface TlsManagedCertificateStatus {
  certificate: string
  directoryUrl: string
  challenge: 'http01' | 'dns01' | 'tls_alpn01'
  dnsProvider: string | null
  keyType: string
  allowedDnsSuffixes: string[]
  diskRevision: string
  activeRevision: string
  notBeforeUnixSeconds: number | null
  notAfterUnixSeconds: number | null
  nextActionUnixSeconds: number | null
  notAfter: string
  jobStatus: 'queued' | 'running' | 'waiting_for_challenge' | 'finalizing' | 'paused' | 'succeeded' | 'failed' | 'cancelled' | null
  jobId: string | null
  paused: boolean
  retainedRevisions: number
  retentionDays: number
  retryAttempt: number
  lastSuccessUnixSeconds: number | null
  lastOutcome: string | null
  lastErrorCode: string | null
}

export interface TlsCertificateInventory {
  name: string
  dnsNames: string[]
  source: 'files' | 'certbot' | 'acme_managed' | 'self_signed_development'
  developmentOnly: boolean
  status: TlsMaterialStatus | TlsManagedCertificateStatus | null
}

export interface TlsInventory {
  certificates: TlsCertificateInventory[]
  watcher: CertbotWatcherSnapshot | null
}

export interface ImportReportRange {
  start: number
  end: number
}

export interface ImportReportSpan {
  sourceId: number
  range: ImportReportRange
}

export interface ImportReportSourceRoot {
  ordinal: number
  path: string | null
  sourceIds: number[]
  outcome: string | null
}

export interface ImportReportSourceReference {
  id: number
  name: string
  path: string | null
  byteLength: number
  fingerprintSha256: string
}

export interface ImportReportDependency {
  sourceId: number
  targetSourceId: number | null
  kind: string
  requestedPath: string | null
  canonicalPath: string | null
  optional: boolean | null
  status: string
  span: ImportReportSpan | null
  failureCode: string | null
  fingerprintSha256: string | null
  truncated: boolean
}

export interface ImportReportSourceGraph {
  roots: ImportReportSourceRoot[]
  sources: ImportReportSourceReference[]
  dependencies: ImportReportDependency[]
  dependenciesComplete: boolean
  snapshotStable: boolean | null
}

export interface ImportReportInactiveSource {
  condition: string
  origin: ImportReportSpan
}

export interface ImportReportSourceMapSegment {
  generated: ImportReportRange
  original: ImportReportRange
}

export interface ImportReportSourceMap {
  sourceId: number
  segments: ImportReportSourceMapSegment[]
}

export interface ImportReportSourceMetadata {
  environmentFingerprintSha256: string | null
  inactiveSources: ImportReportInactiveSource[]
  originalSourceIds: number[]
  sourceMaps: ImportReportSourceMap[]
}

export interface ImportReportCapabilityProfile {
  id: string
  version: number
}

export interface ImportReportSource {
  product: string
  version: string | null
  versionSource: string | null
  capabilityProfile: ImportReportCapabilityProfile
}

export interface ImportReportCandidateDraft {
  version: number
  maxConnections: number | null
  management: boolean
  stats: boolean
  certificates: number
  tlsProfiles: number
  listeners: number
  upstreamPools: number
  httpServices: number
  cacheStores: number
  forwardProxyServices: number
  rtmpServices: number
  l4Services: number
}

export interface ImportReportOrigin {
  role: string | null
  sourceId: number
  range: ImportReportRange | null
  path: string | null
  line: number | null
  includeStack: ImportReportSpan[]
}

export interface ImportReportProvenance {
  path: string
  origins: ImportReportOrigin[]
}

interface ImportReportCandidateEvidence {
  draft: ImportReportCandidateDraft
  provenance: ImportReportProvenance[]
}

export type ImportReportCandidate = ImportReportCandidateEvidence & (
  | { finalized: true; config: CanonicalConfig }
  | { finalized: false; config: null }
)

export interface ImportReportBlocker {
  id: string
  kind: string
  code: string
  message: string
  scope: string | null
  occurrenceIds: string[]
  origins: ImportReportSpan[]
}

export interface ImportReportRequirement {
  kind: string
  directive: string
  values: string[]
  origins: ImportReportOrigin[]
  equivalentRuntimeEndpoint: boolean | null
}

export interface ImportReportRequirements {
  deployment: ImportReportRequirement[]
  activation: ImportReportRequirement[]
}

export interface ImportReportOverlay {
  id: string
  kind: string
  origin: ImportReportOrigin | null
  redactedEvidence: boolean
  values: string[]
  satisfied: boolean
}

export interface ImportReportDiagnostic {
  code: string
  severity: string
  stage: string
  message: string
  primarySpan: ImportReportSpan | null
  includeStack: ImportReportSpan[]
  relatedSpans: Array<{ span: ImportReportSpan; message: string }>
  help: string | null
}

export interface ImportReportEnvelope {
  schemaVersion: 1
  source: ImportReportSource
  sourceGraph: ImportReportSourceGraph
  sourceMetadata: ImportReportSourceMetadata
  candidate: ImportReportCandidate
  blockers: ImportReportBlocker[]
  requirements: ImportReportRequirements
  overlays: ImportReportOverlay[]
  diagnostics: ImportReportDiagnostic[]
  capabilities?: Record<string, unknown>
}

export type ImportReportStatus = 'draft' | 'finalized' | 'blocked'

export interface ImportReportSummary {
  index: number
  product: string
  version: string | null
  versionSource: string | null
  capabilityProfile: ImportReportCapabilityProfile
  status: ImportReportStatus
  rootCount: number
  sourceCount: number
  dependencyCount: number
  blockerCount: number
  diagnosticCount: number
  provenanceCount: number
  requirementCount: number
  overlayCount: number
  previewAvailable: boolean
}

export interface ImportReportPreview {
  format: 'kdl'
  text: string
}

export interface ImportReportResponse {
  schemaVersion: 1
  diskRevision: string
  candidateRevision: string
  activeRevision: string | null
  configFormat: ConfigFormat
  compositional: boolean
  reports: ImportReportSummary[]
  selection: { index: number } | null
  report: ImportReportEnvelope | null
  preview: ImportReportPreview | null
  diagnostics: ConfigDiagnostic[]
}

export interface TlsOperationOutcome {
  certificate: string
  outcome: string
  previousArchiveRevision?: string | null
  archiveRevision?: string
  diskRevision?: string
  activeRevision?: string
  jobId?: string | null
}

export interface TlsReconcileResponse {
  outcomes: TlsOperationOutcome[]
}

export interface TlsRenewResponse extends TlsOperationOutcome {
  diskRevision: string
  activeRevision: string
}

export interface TlsActionResponse extends TlsOperationOutcome {
  jobId: string | null
}

const EVENT_PAGE_PATH_V1 = '/api/v1/events'
const EVENT_PAGE_PATH_V2 = '/api/v2/events'
const EVENT_STREAM_PATH_V1 = '/api/v1/events/stream'
const EVENT_STREAM_PATH_V2 = '/api/v2/events/stream'
const DEFAULT_EVENT_STREAM_MAX_RETRIES = 5
const DEFAULT_EVENT_STREAM_RETRY_DELAY_MS = 250
const DEFAULT_EVENT_STREAM_MAX_RETRY_DELAY_MS = 5_000
const OPERATIONAL_EVENT_NAMES: readonly OperationalEventName[] = [
  'generation_prepare',
  'generation_activate',
  'generation_rollback',
  'generation_drain',
  'generation_start',
  'configuration_reload',
  'import_completed',
  'control_operation',
  'process_shutdown',
  'listener_administrative_state',
  'pool_administrative_state',
  'server_update',
  'rtmp_connect',
  'rtmp_publish',
  'rtmp_play',
  'rtmp_disconnect',
  'rtmp_access',
  'certificate_renewal',
  'certificate_activated',
  'certificate_revocation',
  'certificate_deletion',
  'certificate_account_rollover',
  'certificate_job_control',
  'upstream_endpoint_ejection',
  'upstream_endpoint_recovery',
  'unknown',
]
const OPERATIONAL_EVENT_OUTCOMES: {
  [Event in OperationalEventSimpleName]: readonly OperationalEventSimpleOutcomes[Event][]
} = {
  generation_prepare: ['prepared', 'rejected', 'requested', 'failed'],
  generation_activate: ['activated'],
  generation_rollback: ['prepared', 'rejected', 'requested', 'failed'],
  generation_drain: ['rejected', 'requested', 'failed'],
  generation_start: ['quarantined'],
  configuration_reload: ['rejected', 'applied', 'failed'],
  import_completed: ['applied'],
  control_operation: ['rejected', 'requested', 'failed'],
  process_shutdown: ['rejected', 'requested', 'failed'],
  listener_administrative_state: ['rejected', 'applied', 'failed'],
  pool_administrative_state: ['rejected', 'applied', 'failed'],
  server_update: ['rejected', 'applied', 'failed'],
  rtmp_connect: ['rejected', 'applied', 'failed'],
  rtmp_publish: ['rejected', 'applied', 'failed'],
  rtmp_play: ['rejected', 'applied', 'failed'],
  rtmp_disconnect: ['rejected', 'applied', 'failed'],
  rtmp_access: ['rejected', 'applied', 'failed'],
  certificate_renewal: ['rejected', 'requested', 'applied', 'failed'],
  certificate_activated: ['activated'],
  certificate_revocation: ['rejected', 'requested', 'applied', 'failed'],
  certificate_deletion: ['rejected', 'requested', 'applied', 'failed'],
  certificate_account_rollover: ['rejected', 'requested', 'applied', 'failed'],
  certificate_job_control: ['rejected', 'requested', 'applied', 'failed'],
  unknown: ['unknown'],
}

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

function parseOperationalEvent(
  value: Record<string, unknown>,
  expectedName?: OperationalEventName,
  expectedCursor?: number | null,
  expectedId?: number | null,
): OperationalEvent | null {
  const cursor = eventCursor(value, 'cursor')
  const timestampUnixMs = value.timestampUnixMs === null
    ? null
    : safeInteger(value.timestampUnixMs)
      ? value.timestampUnixMs
      : undefined
  const eventName = value.event === 'certificate_activation'
    ? 'certificate_activated'
    : value.event
  const event = isOperationalEventName(eventName) ? eventName : null
  const outcome = event === null ? null : parseOperationalEventOutcome(event, value.outcome)
  const revision = value.revision === null
    ? null
    : typeof value.revision === 'string'
      ? value.revision
      : undefined
  const certificate = value.certificate === undefined
    ? undefined
    : typeof value.certificate === 'string'
      ? value.certificate
      : null
  if (cursor === null || (expectedCursor !== undefined && cursor !== expectedCursor) ||
    (expectedId !== undefined && expectedId !== cursor) || timestampUnixMs === undefined ||
    event === null || (expectedName !== undefined && event !== expectedName) || outcome === null ||
    revision === undefined || certificate === null) return null
  const common = {
    cursor,
    timestampUnixMs,
    revision,
    ...(certificate === undefined ? {} : { certificate }),
  }
  if (event === 'upstream_endpoint_ejection') {
    return typeof outcome !== 'string' && outcome.type === 'ejected'
      ? { ...common, event, outcome }
      : null
  }
  if (event === 'upstream_endpoint_recovery') {
    return typeof outcome !== 'string' && outcome.type === 'recovered'
      ? { ...common, event, outcome }
      : null
  }
  return typeof outcome === 'string' ? { ...common, event, outcome } as OperationalEvent : null
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

function parseOperationalEventOutcome(
  event: OperationalEventName,
  value: unknown,
): OperationalEventOutcome | null {
  if (event === 'upstream_endpoint_ejection' && isRecord(value) &&
    typeof value.pool === 'string' && typeof value.server === 'string' &&
    value.type === 'ejected' && isHealthFailure(value.reason) && safeInteger(value.failureCount) &&
    safeInteger(value.ejectionCount) && safeInteger(value.ejectedAtUnixMs) &&
    safeInteger(value.ejectionUntilUnixMs)
  ) {
    return {
      type: 'ejected',
      pool: value.pool,
      server: value.server,
      reason: value.reason,
      failureCount: value.failureCount,
      ejectionCount: value.ejectionCount,
      ejectedAtUnixMs: value.ejectedAtUnixMs,
      ejectionUntilUnixMs: value.ejectionUntilUnixMs,
    }
  }
  if (event === 'upstream_endpoint_recovery' && isRecord(value) &&
    typeof value.pool === 'string' && typeof value.server === 'string' &&
    value.type === 'recovered' && (value.reason === null || isHealthFailure(value.reason)) &&
    safeInteger(value.recoveryCount) && safeInteger(value.recoveredAtUnixMs)
  ) {
    return {
      type: 'recovered',
      pool: value.pool,
      server: value.server,
      reason: value.reason,
      recoveryCount: value.recoveryCount,
      recoveredAtUnixMs: value.recoveredAtUnixMs,
    }
  }
  if (event === 'upstream_endpoint_ejection' || event === 'upstream_endpoint_recovery' ||
    typeof value !== 'string'
  ) return null
  return (OPERATIONAL_EVENT_OUTCOMES[event] as readonly string[]).includes(value)
    ? value as OperationalEventSimpleOutcome
    : null
}

function isHealthFailure(value: unknown): value is HealthFailure {
  return typeof value === 'string' &&
    ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'].includes(value)
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
  return parseRtmpCatalog(await request('/api/v1/rtmp/streams', {
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchRtmpStats(signal?: AbortSignal, token?: string): Promise<RtmpStats> {
  return parseRtmpStats(await request('/api/v1/rtmp/stats', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchMonitoring(signal?: AbortSignal, token?: string): Promise<MonitoringSnapshot> {
  return parseMonitoring(await request('/api/v1/monitoring', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchTopology(signal?: AbortSignal, token?: string): Promise<TopologySnapshot> {
  return parseTopology(await request('/api/v1/topology', {
    cache: 'no-store',
    headers: token ? authorizationHeader(token) : undefined,
    signal,
  }))
}

export async function fetchStatus(token: string, signal?: AbortSignal): Promise<RuntimeStatus> {
  return parseRuntimeStatus(await request('/api/v1/status', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchListeners(token: string, signal?: AbortSignal): Promise<ListenerInventoryResponse> {
  return parseListenerInventory(await request('/api/v1/listeners', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchPools(token: string, signal?: AbortSignal): Promise<PoolInventoryResponse> {
  return parsePoolInventory(await request('/api/v1/pools', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchServers(token: string, signal?: AbortSignal): Promise<ServerInventoryResponse> {
  return parseServerInventory(await request('/api/v1/servers', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchGenerations(token: string, signal?: AbortSignal): Promise<GenerationResponse> {
  return parseGenerationResponse(await request('/api/v1/generations', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchEvents(
  after: number,
  limit: number,
  token: string,
  signal?: AbortSignal,
): Promise<EventPage> {
  const options = {
    cache: 'no-store' as const,
    headers: authorizationHeader(token),
    signal,
  }
  try {
    return parseEventPage(
      await request(`${EVENT_PAGE_PATH_V2}?after=${after}&limit=${limit}`, options),
      true,
    )
  } catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404) throw error
    return parseEventPage(
      await request(`${EVENT_PAGE_PATH_V1}?after=${after}&limit=${limit}`, options),
      false,
    )
  }
}

export interface AuditQuery {
  after: number
  limit: number
  category?: AuditCategory
  result?: AuditResult
}

export async function fetchAudit(
  query: AuditQuery,
  token: string,
  signal?: AbortSignal,
): Promise<AuditPage> {
  const params = new URLSearchParams({ after: String(query.after), limit: String(query.limit) })
  if (query.category !== undefined) params.set('category', query.category)
  if (query.result !== undefined) params.set('result', query.result)
  return parseAuditPage(await request(`/api/v1/audit?${params.toString()}`, {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchAuditStatus(token: string, signal?: AbortSignal): Promise<AuditStatusResponse> {
  return parseAuditStatus(await request('/api/v1/audit/status', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function fetchTlsInventory(token: string, signal?: AbortSignal): Promise<TlsInventory> {
  return parseTlsInventory(await request('/api/v1/tls', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }))
}

export async function setListenerAdministrativeState(
  listeners: string[], state: AdministrativeState, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/listeners/administrative-state', { listeners, state }, expectedActiveRevision, token)
}

export async function setPoolAdministrativeState(
  pools: string[], state: AdministrativeState, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/pools/administrative-state', { pools, state }, expectedActiveRevision, token)
}

export async function setServerAdministrativeState(
  targets: ServerTarget[], state: AdministrativeState, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/servers/administrative-state', { targets, state }, expectedActiveRevision, token)
}

export async function setServerHealthOverride(
  targets: ServerTarget[], health: HealthOverride, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/servers/health-override', { targets, health }, expectedActiveRevision, token)
}

export async function setServerChecks(
  targets: ServerTarget[], enabled: boolean, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/servers/checks', { targets, enabled }, expectedActiveRevision, token)
}

export async function setServerMaxConnections(
  targets: ServerTarget[], maxConnections: number | null, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return putRevisionMutation('/api/v1/servers/max-connections', { targets, maxConnections }, expectedActiveRevision, token)
}

export async function refreshServerDns(
  targets: ServerTarget[], expectedActiveRevision: string, token: string,
): Promise<DnsRefreshResponse> {
  return parseDnsRefreshResponse(await request('/api/v1/servers/refresh-dns', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ targets, expectedActiveRevision }),
  }))
}

export async function reloadGeneration(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/reload', {}, expectedActiveRevision, token)
}

export async function rollbackGeneration(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/rollback', {}, expectedActiveRevision, token)
}

export async function drainGeneration(
  expectedActiveRevision: string, token: string, timeoutMs?: number,
): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/generations/drain', timeoutMs === undefined ? {} : { timeoutMs }, expectedActiveRevision, token)
}

export async function reconcileTls(
  expectedActiveRevision: string, token: string, certificate?: string,
): Promise<TlsReconcileResponse> {
  return parseTlsReconcileResponse(await request('/api/v1/tls/reconcile', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, ...(certificate ? { certificate } : {}) }),
  }))
}

export async function renewManagedCertificate(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsRenewResponse> {
  return parseTlsRenewResponse(await request('/api/v1/tls/renew', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}

export async function revokeManagedCertificate(
  expectedActiveRevision: string,
  token: string,
  certificate: string,
  reason?: number,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/revoke', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate, ...(reason === undefined ? {} : { reason }) }),
  }))
}

export async function deleteManagedCertificate(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/delete', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}

export async function rolloverManagedAccountKey(
  expectedActiveRevision: string,
  token: string,
  certificate?: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request('/api/v1/tls/account/rollover', {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, ...(certificate ? { certificate } : {}) }),
  }))
}

export async function cancelManagedJob(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/cancel', expectedActiveRevision, token, certificate)
}

export async function pauseManagedJobs(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/pause', expectedActiveRevision, token, certificate)
}

export async function resumeManagedJobs(
  expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return postTlsAction('/api/v1/tls/jobs/resume', expectedActiveRevision, token, certificate)
}

export async function drainProcess(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/process/drain', {}, expectedActiveRevision, token)
}

export async function shutdownProcess(expectedActiveRevision: string, token: string): Promise<MutationResponse> {
  return postRevisionMutation('/api/v1/process/shutdown', {}, expectedActiveRevision, token)
}

export async function fetchConfig(token: string, signal?: AbortSignal): Promise<ConfigSnapshot> {
  return parseConfigSnapshot(await request('/api/v1/config', {
    cache: 'no-store',
    headers: authorizationHeader(token),
    signal,
  }, 200))
}

export async function fetchImportReports(
  token: string,
  index?: number,
  signal?: AbortSignal,
): Promise<ImportReportResponse> {
  const path = index === undefined ? '/api/v1/import-reports' : `/api/v1/import-reports/${index}`
  return parseImportReportResponse(await request(path, {
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
  return parseValidation(await request('/api/v1/config/validate', {
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
  return parseSave(await request('/api/v1/config', {
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
  return parseRecorder(await request(
    `/api/v1/rtmp/streams/${streamId}/recorders/${recorderId}/${action}`,
    { method: 'POST', headers },
  ))
}

export async function dropRtmpClient(
  client: RtmpClientSnapshot,
  target: RtmpClientControlTarget,
  token: string,
): Promise<RtmpControlResponse> {
  const suffix = target === 'client' ? 'drop' : `${target}/drop`
  return parseRtmpControlResponse(await request(
    `/api/v1/rtmp/clients/${client.id}/${suffix}`,
    {
      method: 'POST',
      headers: {
        ...authorizationHeader(token),
        'If-Rtmp-Session-Revision': client.revision,
      },
    },
    202,
  ))
}

async function postRevisionMutation(
  url: string, body: Record<string, unknown>, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return parseMutationResponse(await request(url, {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ ...body, expectedActiveRevision }),
  }))
}

async function putRevisionMutation(
  url: string, body: Record<string, unknown>, expectedActiveRevision: string, token: string,
): Promise<MutationResponse> {
  return parseMutationResponse(await request(url, {
    method: 'PUT', headers: mutationHeaders(token),
    body: JSON.stringify({ ...body, expectedActiveRevision }),
  }))
}

async function postTlsAction(
  url: string, expectedActiveRevision: string, token: string, certificate: string,
): Promise<TlsActionResponse> {
  return parseTlsActionResponse(await request(url, {
    method: 'POST', headers: mutationHeaders(token),
    body: JSON.stringify({ expectedActiveRevision, certificate }),
  }))
}

function mutationHeaders(token: string): Record<string, string> {
  return { ...authorizationHeader(token), 'Content-Type': 'application/json' }
}

async function fetchActiveRevision(token: string): Promise<string> {
  const value = parseGenerationResponse(await request('/api/v1/generations', {
    cache: 'no-store',
    headers: authorizationHeader(token),
  }))
  if (value.generation.activeRevision === null) {
    throw new Error('generation response has no active revision')
  }
  return value.generation.activeRevision
}

function authorizationHeader(token: string): Record<string, string> {
  return { Authorization: `Bearer ${token}` }
}


function parseGenerationResponse(value: unknown): GenerationResponse {
  if (!isRecord(value) || !generationStatus(value.generation)) return invalidPayload('generation status')
  return { generation: value.generation }
}

function parseEventPage(value: unknown, requireLatestCursor: boolean): EventPage {
  const latestCursor = isRecord(value) && value.latestCursor === undefined && !requireLatestCursor
    ? value.cursor
    : isRecord(value)
      ? value.latestCursor
      : undefined
  if (!isRecord(value) || !Array.isArray(value.events) ||
    !value.events.every((event) => isRecord(event) && parseOperationalEvent(event) !== null) ||
    !safeInteger(value.cursor) || !safeInteger(latestCursor) || typeof value.hasMore !== 'boolean' ||
    !(value.oldestCursor === null || safeInteger(value.oldestCursor))
  ) return invalidPayload('event history')
  const events = value.events.map((event) => parseOperationalEvent(event as Record<string, unknown>))
  if (events.some((event): event is null => event === null)) return invalidPayload('event history')
  return {
    events: events as OperationalEvent[],
    cursor: value.cursor,
    latestCursor,
    hasMore: value.hasMore,
    oldestCursor: value.oldestCursor,
  }
}

function parseAuditPage(value: unknown): AuditPage {
  if (!isRecord(value) || !Array.isArray(value.records) ||
    !value.records.every((record) => auditRecord(record) !== null) ||
    !safeInteger(value.cursor) || !safeInteger(value.latestCursor) ||
    typeof value.hasMore !== 'boolean' ||
    !(value.oldestCursor === null || safeInteger(value.oldestCursor))
  ) return invalidPayload('audit history')
  const records = value.records.map((record) => parseAuditRecord(record as Record<string, unknown>))
  if (records.some((record): record is null => record === null)) return invalidPayload('audit history')
  return {
    records: records as AuditRecord[],
    cursor: value.cursor,
    latestCursor: value.latestCursor,
    hasMore: value.hasMore,
    oldestCursor: value.oldestCursor,
  }
}

function parseAuditStatus(value: unknown): AuditStatusResponse {
  if (!isRecord(value) || !auditStatus(value.audit)) return invalidPayload('audit status')
  const status = value.audit as Record<string, unknown>
  return {
    audit: {
      state: status.state as AuditComponentState,
      persistent: status.persistent as boolean,
      degraded: status.degraded as boolean,
      recordCount: status.recordCount as number,
      bytes: status.bytes as number,
      rotatedFiles: status.rotatedFiles as number,
      maxRecords: status.maxRecords as number,
      maxRecordBytes: status.maxRecordBytes as number,
      maxFileBytes: status.maxFileBytes as number,
      maxTotalBytes: status.maxTotalBytes as number,
      maxRotatedFiles: status.maxRotatedFiles as number,
      writeFailures: status.writeFailures as number,
      corruptRecords: status.corruptRecords as number,
      ...(status.lastError === undefined ? {} : { lastError: status.lastError as string }),
    },
  }
}

function parseAuditRecord(value: Record<string, unknown>): AuditRecord | null {
  if (!auditRecord(value)) return null
  return {
    id: value.id as number,
    timestampUnixMs: value.timestampUnixMs as number,
    correlationId: value.correlationId as string,
    actor: value.actor as string,
    source: value.source as string,
    category: value.category as AuditCategory,
    operation: value.operation as string,
    result: value.result as AuditResult,
    ...(value.revision === undefined ? {} : { revision: value.revision as string }),
  }
}

function auditRecord(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && safeInteger(value.id) && safeInteger(value.timestampUnixMs) &&
    typeof value.correlationId === 'string' && typeof value.actor === 'string' &&
    typeof value.source === 'string' && isAuditCategory(value.category) &&
    typeof value.operation === 'string' && isAuditResult(value.result) &&
    (value.revision === undefined || typeof value.revision === 'string')
}

function auditStatus(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && ['healthy', 'degraded', 'memory'].includes(String(value.state)) &&
    typeof value.persistent === 'boolean' && typeof value.degraded === 'boolean' &&
    safeInteger(value.recordCount) && safeInteger(value.bytes) && safeInteger(value.rotatedFiles) &&
    safeInteger(value.maxRecords) && safeInteger(value.maxRecordBytes) &&
    safeInteger(value.maxFileBytes) && safeInteger(value.maxTotalBytes) &&
    safeInteger(value.maxRotatedFiles) && safeInteger(value.writeFailures) &&
    safeInteger(value.corruptRecords) &&
    (value.lastError === undefined || typeof value.lastError === 'string')
}

function isAuditCategory(value: unknown): value is AuditCategory {
  return ['reload', 'import', 'certificate', 'control'].includes(String(value))
}

function isAuditResult(value: unknown): value is AuditResult {
  return ['requested', 'succeeded', 'failed', 'rejected', 'conflict', 'partial', 'degraded']
    .includes(String(value))
}

function parseTlsInventory(value: unknown): TlsInventory {
  if (!isRecord(value) || !Array.isArray(value.certificates) ||
    !value.certificates.every(tlsCertificateInventory) ||
    !(value.watcher === null || certbotWatcher(value.watcher))
  ) return invalidPayload('TLS inventory')
  return {
    certificates: value.certificates.map((certificate) => normalizeTlsCertificate(certificate as Record<string, unknown>)),
    watcher: value.watcher as CertbotWatcherSnapshot | null,
  }
}

function parseMutationResponse(value: unknown): MutationResponse {
  if (!isRecord(value) || typeof value.outcome !== 'string' ||
    (value.changed !== undefined && !safeInteger(value.changed))
  ) return invalidPayload('management mutation')
  return { outcome: value.outcome, ...(value.changed === undefined ? {} : { changed: value.changed }) }
}

function parseDnsRefreshResponse(value: unknown): DnsRefreshResponse {
  if (!isRecord(value) || !['refreshed', 'partially_refreshed'].includes(String(value.outcome)) ||
    value.atomic !== false || !Array.isArray(value.servers) || !value.servers.every(dnsRefreshResult)
  ) return invalidPayload('DNS refresh')
  return {
    outcome: value.outcome as DnsRefreshResponse['outcome'],
    atomic: false,
    servers: value.servers.map((server) => normalizeDnsRefreshResult(server as Record<string, unknown>)),
  }
}

function parseTlsReconcileResponse(value: unknown): TlsReconcileResponse {
  if (!isRecord(value) || !Array.isArray(value.outcomes) || !value.outcomes.every(tlsOperationOutcome)) {
    return invalidPayload('TLS reconciliation')
  }
  return { outcomes: value.outcomes.map((outcome) => normalizeTlsOperationOutcome(outcome as Record<string, unknown>)) }
}

function parseTlsRenewResponse(value: unknown): TlsRenewResponse {
  if (!tlsOperationOutcome(value) || typeof value.diskRevision !== 'string' ||
    typeof value.activeRevision !== 'string'
  ) return invalidPayload('TLS renewal')
  return {
    ...normalizeTlsOperationOutcome(value),
    diskRevision: value.diskRevision,
    activeRevision: value.activeRevision,
  }
}

function parseTlsActionResponse(value: unknown): TlsActionResponse {
  if (!tlsOperationOutcome(value)) return invalidPayload('TLS action')
  return {
    ...normalizeTlsOperationOutcome(value),
    jobId: value.jobId as string | null | undefined ?? null,
  }
}

function nullableRevision(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

function generationStatus(value: unknown): value is GenerationStatus {
  return isRecord(value) && typeof value.buildVersion === 'string' &&
    nullableRevision(value.diskRevision) && nullableRevision(value.candidateRevision) &&
    nullableRevision(value.activeRevision) && nullableRevision(value.previousRevision) &&
    nullableRevision(value.quarantinedRevision) && typeof value.activeAccepting === 'boolean' &&
    typeof value.degraded === 'boolean' && nullableString(value.lastFailure) &&
    safeInteger(value.prepares) && safeInteger(value.activations) && safeInteger(value.failures) &&
    safeInteger(value.rollbacks)
}

function tlsCertificateInventory(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value) || typeof value.name !== 'string' || !Array.isArray(value.dnsNames) ||
    !value.dnsNames.every((name) => typeof name === 'string') ||
    !['files', 'certbot', 'acme_managed', 'self_signed_development'].includes(String(value.source)) ||
    typeof value.developmentOnly !== 'boolean'
  ) return false
  if (value.status === null) return true
  if (!isRecord(value.status)) return false
  return value.source === 'acme_managed' ? tlsManagedCertificateStatus(value.status) : tlsMaterialStatus(value.status)
}

function tlsMaterialStatus(value: unknown): value is Record<string, unknown> & TlsMaterialStatus {
  return isRecord(value) && typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    (value.activeArchiveRevision === undefined || safeInteger(value.activeArchiveRevision)) &&
    (value.lastOutcome === undefined || nullableString(value.lastOutcome)) &&
    (value.lastErrorCode === undefined || nullableString(value.lastErrorCode))
}

function tlsManagedCertificateStatus(value: unknown): value is Record<string, unknown> & TlsManagedCertificateStatus {
  return isRecord(value) && typeof value.certificate === 'string' && typeof value.directoryUrl === 'string' &&
    ['http01', 'dns01', 'tls_alpn01'].includes(String(value.challenge)) && nullableString(value.dnsProvider) &&
    typeof value.keyType === 'string' && Array.isArray(value.allowedDnsSuffixes) && value.allowedDnsSuffixes.every((suffix) => typeof suffix === 'string') &&
    typeof value.diskRevision === 'string' && typeof value.activeRevision === 'string' &&
    nullableSafeInteger(value.notBeforeUnixSeconds) && nullableSafeInteger(value.notAfterUnixSeconds) &&
    nullableSafeInteger(value.nextActionUnixSeconds) && typeof value.notAfter === 'string' &&
     (value.jobStatus === null || ['queued', 'running', 'waiting_for_challenge', 'finalizing', 'paused', 'succeeded', 'failed', 'cancelled'].includes(String(value.jobStatus))) &&
     nullableString(value.jobId) && typeof value.paused === 'boolean' &&
     safeInteger(value.retainedRevisions) && safeInteger(value.retentionDays) &&
    safeInteger(value.retryAttempt) && nullableSafeInteger(value.lastSuccessUnixSeconds) &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function normalizeTlsCertificate(value: Record<string, unknown>): TlsCertificateInventory {
  const status = value.status === null
    ? null
    : value.source === 'acme_managed'
      ? normalizeTlsManagedStatus(value.status as Record<string, unknown>)
      : normalizeTlsMaterialStatus(value.status as Record<string, unknown>)
  return {
    name: value.name as string,
    dnsNames: value.dnsNames as string[],
    source: value.source as TlsCertificateInventory['source'],
    developmentOnly: value.developmentOnly as boolean,
    status,
  }
}

function normalizeTlsMaterialStatus(value: Record<string, unknown>): TlsMaterialStatus {
  return {
    activeContentRevision: value.activeContentRevision as string,
    expiresAt: value.expiresAt as string,
    ...(value.activeArchiveRevision === undefined ? {} : { activeArchiveRevision: value.activeArchiveRevision as number }),
    lastOutcome: value.lastOutcome as string | null | undefined ?? null,
    lastErrorCode: value.lastErrorCode as string | null | undefined ?? null,
  }
}

function normalizeTlsManagedStatus(value: Record<string, unknown>): TlsManagedCertificateStatus {
  return {
    certificate: value.certificate as string,
    directoryUrl: value.directoryUrl as string,
    challenge: value.challenge as TlsManagedCertificateStatus['challenge'],
    dnsProvider: value.dnsProvider as string | null,
    keyType: value.keyType as string,
    allowedDnsSuffixes: value.allowedDnsSuffixes as string[],
    diskRevision: value.diskRevision as string,
    activeRevision: value.activeRevision as string,
    notBeforeUnixSeconds: value.notBeforeUnixSeconds as number | null,
    notAfterUnixSeconds: value.notAfterUnixSeconds as number | null,
    nextActionUnixSeconds: value.nextActionUnixSeconds as number | null,
    notAfter: value.notAfter as string,
    jobStatus: value.jobStatus as TlsManagedCertificateStatus['jobStatus'],
    jobId: value.jobId as string | null,
    paused: value.paused as boolean,
    retainedRevisions: value.retainedRevisions as number,
    retentionDays: value.retentionDays as number,
    retryAttempt: value.retryAttempt as number,
    lastSuccessUnixSeconds: value.lastSuccessUnixSeconds as number | null,
    lastOutcome: value.lastOutcome as string | null,
    lastErrorCode: value.lastErrorCode as string | null,
  }
}

function dnsRefreshResult(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && typeof value.pool === 'string' && typeof value.server === 'string' &&
    ['refreshed', 'failed'].includes(String(value.outcome)) &&
    (value.addresses === undefined || (Array.isArray(value.addresses) && value.addresses.every((address) => typeof address === 'string'))) &&
    (value.error === undefined || (isRecord(value.error) && typeof value.error.code === 'string'))
}

function normalizeDnsRefreshResult(value: Record<string, unknown>): DnsRefreshResult {
  return {
    pool: value.pool as string,
    server: value.server as string,
    outcome: value.outcome as DnsRefreshResult['outcome'],
    ...(value.addresses === undefined ? {} : { addresses: value.addresses as string[] }),
    ...(value.error === undefined ? {} : { errorCode: (value.error as Record<string, unknown>).code as string }),
  }
}

function tlsOperationOutcome(value: unknown): value is Record<string, unknown> {
  if (!isRecord(value) || typeof value.certificate !== 'string' || typeof value.outcome !== 'string') return false
  return optionalStringOrNull(value.previousArchiveRevision) && optionalString(value.archiveRevision) &&
    optionalString(value.diskRevision) && optionalString(value.activeRevision) &&
    optionalStringOrNull(value.jobId)
}

function normalizeTlsOperationOutcome(value: Record<string, unknown>): TlsOperationOutcome {
  return {
    certificate: value.certificate as string,
    outcome: value.outcome as string,
    ...(value.previousArchiveRevision === undefined ? {} : { previousArchiveRevision: value.previousArchiveRevision as string | null }),
    ...(value.archiveRevision === undefined ? {} : { archiveRevision: value.archiveRevision as string }),
    ...(value.diskRevision === undefined ? {} : { diskRevision: value.diskRevision as string }),
    ...(value.activeRevision === undefined ? {} : { activeRevision: value.activeRevision as string }),
    jobId: value.jobId as string | null | undefined ?? null,
  }
}

function optionalString(value: unknown): boolean {
  return value === undefined || typeof value === 'string'
}

function optionalStringOrNull(value: unknown): boolean {
  return value === undefined || value === null || typeof value === 'string'
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
  return {
    revision: value.revision,
    as_of_unix_ms: value.as_of_unix_ms,
    capabilities: {
      live_ingest: value.capabilities.live_ingest,
      manual_recording: value.capabilities.manual_recording,
    },
    streams: value.streams.map(projectStreamSnapshot),
  }
}

function parseRtmpStats(value: unknown): RtmpStats {
  if (!isRecord(value) || !decimalString(value.revision) || !safeInteger(value.asOfUnixMs) ||
    !rtmpStatsGlobal(value.global) || !Array.isArray(value.live) || !value.live.every(rtmpLiveStat) ||
    !Array.isArray(value.clients) || !value.clients.every(rtmpClientSnapshot) ||
    typeof value.liveTruncated !== 'boolean' || typeof value.clientsTruncated !== 'boolean'
  ) return invalidPayload('RTMP statistics')
  return value as unknown as RtmpStats
}

function rtmpStatsGlobal(value: unknown): value is RtmpStatsGlobal {
  return isRecord(value) && safeInteger(value.activeStreams) && safeInteger(value.publishers) &&
    safeInteger(value.subscribers) && decimalString(value.audioPayloadBytes) &&
    decimalString(value.videoPayloadBytes) && typeof value.liveIngest === 'boolean' &&
    typeof value.manualRecording === 'boolean'
}

function rtmpLiveStat(value: unknown): value is RtmpLiveStat {
  return isRecord(value) && typeof value.id === 'string' && typeof value.service === 'string' &&
    typeof value.application === 'string' && typeof value.name === 'string' &&
    safeInteger(value.createdAtUnixMs) && nullableString(value.publisherSessionId) &&
    safeInteger(value.subscriberCount) && decimalString(value.audioPayloadBytes) &&
    decimalString(value.videoPayloadBytes)
}

function rtmpClientSnapshot(value: unknown): value is RtmpClientSnapshot {
  return isRecord(value) && typeof value.id === 'string' && typeof value.service === 'string' &&
    nullableString(value.peerIp) && safeInteger(value.connectedAtUnixMs) &&
    nullableString(value.application) && nullableString(value.stream) &&
    (value.role === 'client' || value.role === 'publisher' || value.role === 'subscriber') &&
    decimalString(value.revision)
}

function parseRtmpControlResponse(value: unknown): RtmpControlResponse {
  if (!isRecord(value) || (value.outcome !== 'requested' && value.outcome !== 'already_requested') ||
    typeof value.sessionId !== 'string' ||
    (value.target !== 'client' && value.target !== 'publisher' && value.target !== 'subscriber') ||
    !decimalString(value.sessionRevision)
  ) return invalidPayload('RTMP control response')
  return value as unknown as RtmpControlResponse
}

function parseMonitoring(value: unknown): MonitoringSnapshot {
  if (!isRecord(value) || !safeInteger(value.sampledAtUnixMs) || !safeInteger(value.uptimeMs) ||
    !safeInteger(value.generationAgeMs) ||
    !monitoringProcess(value.process) || !monitoringHost(value.host) || !monitoringTraffic(value.traffic) ||
    !Array.isArray(value.listeners) || !value.listeners.every(monitoringListener) ||
    !Array.isArray(value.upstreamPools) || !value.upstreamPools.every(monitoringPool) ||
    !Array.isArray(value.transportOperations) ||
    !value.transportOperations.every(monitoringTransportOperation) ||
    !Array.isArray(value.accessRecords) || !value.accessRecords.every(monitoringAccessRecord) ||
    !Array.isArray(value.certbotCertificates) || !value.certbotCertificates.every(certbotCertificate) ||
    !(value.certbotWatcher === null || certbotWatcher(value.certbotWatcher)) || !isRecord(value.rtmp) ||
    !Array.isArray(value.acmeManagedCertificates) ||
    !value.acmeManagedCertificates.every(acmeManagedCertificate) ||
    !Array.isArray(value.directFileCertificates) ||
    !value.directFileCertificates.every(directFileCertificate) ||
    !(value.directFileWatcher === null || certbotWatcher(value.directFileWatcher)) ||
    !safeInteger(value.rtmp.activeStreams) || !safeInteger(value.rtmp.publishers) ||
    !safeInteger(value.rtmp.subscribers) || !decimalString(value.rtmp.mediaPayloadBytesReceived) ||
    typeof value.rtmp.recordingSupported !== 'boolean' || typeof value.rtmp.manualRecording !== 'boolean' ||
    !decimalString(value.rtmp.recorderBytesWritten) || !decimalString(value.rtmp.recorderSegmentsStarted) ||
    !decimalString(value.rtmp.recorderSegmentsCompleted) || !decimalString(value.rtmp.recorderDiscontinuities) ||
    !decimalString(value.rtmp.relayConnectionAttempts) || !decimalString(value.rtmp.relayConnections) ||
    !decimalString(value.rtmp.relayReconnects) || !decimalString(value.rtmp.relayDnsRefreshAttempts) ||
    !decimalString(value.rtmp.relayDnsRefreshSuccesses) || !decimalString(value.rtmp.relayDnsRefreshFailures) ||
    !decimalString(value.rtmp.relayEventsSent) ||
    !decimalString(value.rtmp.relayEventsDropped) || !decimalString(value.rtmp.relayPayloadBytesSent) ||
    !monitoringRtmpAccessLog(value.rtmp.accessLog) ||
    !Array.isArray(value.rtmp.relays) || !value.rtmp.relays.every(monitoringRelay) ||
    !Array.isArray(value.rtmp.recorders) || !value.rtmp.recorders.every(monitoringRecorder)
  ) return invalidPayload('monitoring')
  return {
    sampledAtUnixMs: value.sampledAtUnixMs,
    uptimeMs: value.uptimeMs,
    generationAgeMs: value.generationAgeMs,
    process: projectMonitoringProcess(value.process),
    host: projectMonitoringHost(value.host),
    traffic: projectMonitoringTraffic(value.traffic),
    listeners: value.listeners.map(projectMonitoringListener),
    upstreamPools: value.upstreamPools.map(projectMonitoringPool),
    transportOperations: value.transportOperations.map(projectMonitoringTransportOperation),
    accessRecords: value.accessRecords.map(projectMonitoringAccessRecord),
    certbotCertificates: value.certbotCertificates.map(projectCertbotCertificate),
    certbotWatcher: value.certbotWatcher === null ? null : projectCertbotWatcher(value.certbotWatcher),
    acmeManagedCertificates: value.acmeManagedCertificates.map(projectAcmeManagedCertificate),
    directFileCertificates: value.directFileCertificates.map(projectDirectFileCertificate),
    directFileWatcher: value.directFileWatcher === null ? null : projectCertbotWatcher(value.directFileWatcher),
    rtmp: projectMonitoringRtmp(value.rtmp),
  }
}

function projectStreamSnapshot(stream: StreamSnapshot): StreamSnapshot {
  return {
    id: stream.id,
    revision: stream.revision,
    server_id: stream.server_id,
    application: stream.application,
    name: stream.name,
    created_at_unix_ms: stream.created_at_unix_ms,
    publisher: stream.publisher === null ? null : {
      session_id: stream.publisher.session_id,
      attached_at_unix_ms: stream.publisher.attached_at_unix_ms,
    },
    subscriber_count: stream.subscriber_count,
    media: {
      audio: projectTrackSnapshot(stream.media.audio),
      video: projectTrackSnapshot(stream.media.video),
      fanout_payload_bytes: stream.media.fanout_payload_bytes,
    },
    relays: stream.relays.map(projectRelaySnapshot),
    recording_supported: stream.recording_supported,
    manual_recording: stream.manual_recording,
    recorders: stream.recorders.map(projectRecorderSnapshot),
  }
}

function projectTrackSnapshot(track: TrackSnapshot): TrackSnapshot {
  return {
    codec_id: track.codec_id,
    codec_fourcc: track.codec_fourcc,
    codec_name: track.codec_name,
    recording_supported: track.recording_supported,
    payload_bytes: track.payload_bytes,
    last_rtmp_timestamp_ms: track.last_rtmp_timestamp_ms,
    last_observed_at_unix_ms: track.last_observed_at_unix_ms,
  }
}

function projectRelaySnapshot(relay: RelaySnapshot): RelaySnapshot {
  return {
    id: relay.id,
    destination: {
      address: relay.destination.address,
      application: relay.destination.application,
      stream_name: relay.destination.stream_name,
    },
    phase: relay.phase,
    last_failure: relay.last_failure,
    queue_messages: relay.queue_messages,
    queue_bytes: relay.queue_bytes,
    connection_attempts: relay.connection_attempts,
    connections: relay.connections,
    reconnects: relay.reconnects,
    dns_refresh_attempts: relay.dns_refresh_attempts,
    dns_refresh_successes: relay.dns_refresh_successes,
    dns_refresh_failures: relay.dns_refresh_failures,
    last_dns_refresh_failure: relay.last_dns_refresh_failure,
    events_enqueued: relay.events_enqueued,
    events_sent: relay.events_sent,
    events_dropped: relay.events_dropped,
    payload_bytes_sent: relay.payload_bytes_sent,
  }
}

function projectRecorderSnapshot(recorder: RecorderSnapshot): RecorderSnapshot {
  return {
    id: recorder.id,
    name: recorder.name,
    manual: recorder.manual,
    phase: projectRecorderPhase(recorder.phase),
    changed_at_unix_ms: recorder.changed_at_unix_ms,
    bytes_written: recorder.bytes_written,
    current_relative_name: recorder.current_relative_name,
    published_but_not_durable_relative_name: recorder.published_but_not_durable_relative_name,
    segments_started: recorder.segments_started,
    segments_completed: recorder.segments_completed,
    discontinuities: recorder.discontinuities,
    last_completed_relative_name: recorder.last_completed_relative_name,
    recoverable_partial_name: recorder.recoverable_partial_name,
    last_notification: recorder.last_notification,
  }
}

function projectRecorderPhase(phase: RecorderPhase): RecorderPhase {
  switch (phase.state) {
    case 'idle':
      return { state: phase.state }
    case 'starting':
    case 'stopping':
      return { state: phase.state, operation_id: phase.operation_id }
    case 'recording':
      return {
        state: phase.state,
        operation_id: phase.operation_id,
        started_at_unix_ms: phase.started_at_unix_ms,
      }
    case 'failed':
      return { state: phase.state, operation_id: phase.operation_id, code: phase.code }
  }
}

function projectMonitoringProcess(value: MonitoringProcess): MonitoringProcess {
  return {
    activeConnections: value.activeConnections,
    administrativeState: value.administrativeState,
    status: projectMonitoringComponentStatus(value.status),
    cpuPercent: value.cpuPercent,
    maxConnections: value.maxConnections,
    rejectedConnections: value.rejectedConnections,
    retryAttempts: value.retryAttempts,
    residentMemoryBytes: value.residentMemoryBytes,
    virtualMemoryBytes: value.virtualMemoryBytes,
    threadCount: value.threadCount,
    openFileDescriptors: value.openFileDescriptors,
  }
}

function projectMonitoringHost(value: MonitoringHost): MonitoringHost {
  return {
    status: projectMonitoringComponentStatus(value.status),
    loadAverage1m: value.loadAverage1m,
    loadAverage5m: value.loadAverage5m,
    loadAverage15m: value.loadAverage15m,
    totalMemoryBytes: value.totalMemoryBytes,
    availableMemoryBytes: value.availableMemoryBytes,
  }
}

function projectMonitoringComponentStatus(value: MonitoringComponentStatus): MonitoringComponentStatus {
  return {
    state: value.state,
    ...(value.reason === undefined ? {} : { reason: value.reason }),
  }
}

function projectMonitoringTraffic(value: MonitoringTraffic): MonitoringTraffic {
  return {
    acceptedConnections: value.acceptedConnections,
    rejectedConnections: value.rejectedConnections,
    activeConnections: value.activeConnections,
    bytesReceived: value.bytesReceived,
    bytesSent: value.bytesSent,
  }
}

function projectMonitoringListener(value: MonitoringListener): MonitoringListener {
  return {
    administrativeState: value.administrativeState,
    name: value.name,
    protocol: value.protocol,
    bind: value.bind,
    maxConnections: value.maxConnections,
    state: value.state,
    ...projectMonitoringTraffic(value),
    httpOperations: value.httpOperations === null ? null : projectMonitoringHttpOperations(value.httpOperations),
    tcpRelays: value.tcpRelays === null ? null : projectMonitoringTcpRelays(value.tcpRelays),
    proxyProtocol: value.proxyProtocol === null ? null : projectMonitoringProxyProtocol(value.proxyProtocol),
    cache: value.cache === null ? null : projectMonitoringCache(value.cache),
  }
}

function projectMonitoringHttpOperations(value: MonitoringHttpOperations): MonitoringHttpOperations {
  return {
    outcomes: value.outcomes.map((outcome) => ({ result: outcome.result, count: outcome.count })),
    latency: projectMonitoringLatency(value.latency),
  }
}

function projectMonitoringTcpRelays(value: MonitoringTcpRelays): MonitoringTcpRelays {
  return {
    outcomes: value.outcomes.map((outcome) => ({ result: outcome.result, count: outcome.count })),
    latency: projectMonitoringLatency(value.latency),
  }
}

function projectMonitoringLatency(
  value: MonitoringHttpOperations['latency'],
): MonitoringHttpOperations['latency'] {
  return {
    buckets: value.buckets.map((bucket) => ({
      upperBoundMs: bucket.upperBoundMs,
      count: bucket.count,
    })),
    count: value.count,
    sumMs: value.sumMs,
  }
}

function projectMonitoringProxyProtocol(value: MonitoringProxyProtocol): MonitoringProxyProtocol {
  return {
    outcomes: value.outcomes.map((outcome) => ({ result: outcome.result, count: outcome.count })),
  }
}

function projectMonitoringCache(value: MonitoringCache): MonitoringCache {
  return {
    hits: value.hits,
    misses: value.misses,
    admissions: value.admissions,
    evictions: value.evictions,
  }
}

function projectMonitoringPool(value: MonitoringPool): MonitoringPool {
  return {
    name: value.name,
    algorithm: value.algorithm,
    availableEndpoints: value.availableEndpoints,
    totalEndpoints: value.totalEndpoints,
    unavailableSelections: value.unavailableSelections,
    queued: value.queued,
    queuedTotal: value.queuedTotal,
    queueTimeouts: value.queueTimeouts,
    queueCancellations: value.queueCancellations,
    endpoints: value.endpoints.map(projectMonitoringPoolEndpoint),
  }
}

function projectMonitoringPoolEndpoint(value: MonitoringPoolEndpoint): MonitoringPoolEndpoint {
  return {
    activeConnections: value.activeConnections,
    administrativeState: value.administrativeState,
    address: value.address,
    checksEnabled: value.checksEnabled,
    checksRunning: value.checksRunning,
    configuredMaxConnections: value.configuredMaxConnections,
    healthOverride: value.healthOverride,
    maxConnections: value.maxConnections,
    name: value.name,
    state: value.state,
    weight: value.weight,
    lastCheckedAtUnixMs: value.lastCheckedAtUnixMs,
    lastTransitionAtUnixMs: value.lastTransitionAtUnixMs,
    successfulChecks: value.successfulChecks,
    failedChecks: value.failedChecks,
    consecutiveSuccesses: value.consecutiveSuccesses,
    consecutiveFailures: value.consecutiveFailures,
    lastFailure: value.lastFailure,
    passiveEjected: value.passiveEjected,
    passiveFailureCount: value.passiveFailureCount,
    passiveConsecutiveFailures: value.passiveConsecutiveFailures,
    passiveEjectionCount: value.passiveEjectionCount,
    passiveEjectionReason: value.passiveEjectionReason,
    passiveEjectedAtUnixMs: value.passiveEjectedAtUnixMs,
    passiveEjectionUntilUnixMs: value.passiveEjectionUntilUnixMs,
    passiveRecoveryCount: value.passiveRecoveryCount,
    passiveLastRecoveryAtUnixMs: value.passiveLastRecoveryAtUnixMs,
  }
}

function projectMonitoringTransportOperation(
  value: MonitoringTransportOperation,
): MonitoringTransportOperation {
  return {
    transport: value.transport,
    outcomes: value.outcomes.map((outcome) => ({ outcome: outcome.outcome, count: outcome.count })),
    latency: projectMonitoringLatency(value.latency),
  }
}

function projectMonitoringAccessRecord(value: MonitoringAccessRecord): MonitoringAccessRecord {
  return {
    timestampUnixMs: value.timestampUnixMs,
    correlationId: value.correlationId,
    listener: value.listener,
    transport: value.transport,
    outcome: value.outcome,
    durationMs: value.durationMs,
    bytesReceived: value.bytesReceived,
    bytesSent: value.bytesSent,
  }
}

function projectCertbotCertificate(value: CertbotCertificateSnapshot): CertbotCertificateSnapshot {
  return {
    name: value.name,
    activeArchiveRevision: value.activeArchiveRevision,
    activeContentRevision: value.activeContentRevision,
    expiresAt: value.expiresAt,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
  }
}

function projectAcmeManagedCertificate(
  value: AcmeManagedCertificateSnapshot,
): AcmeManagedCertificateSnapshot {
  return {
    name: value.name,
    directoryUrl: value.directoryUrl,
    diskRevision: value.diskRevision,
    activeRevision: value.activeRevision,
    expiresAt: value.expiresAt,
    notBeforeUnixSeconds: value.notBeforeUnixSeconds,
    notAfterUnixSeconds: value.notAfterUnixSeconds,
    nextActionUnixSeconds: value.nextActionUnixSeconds,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
    renewalInformationStatus: value.renewalInformationStatus,
    dnsProvider: value.dnsProvider,
    dnsProviderDeployment: value.dnsProviderDeployment,
    dnsProviderHealth: value.dnsProviderHealth,
    dnsCleanupStatus: value.dnsCleanupStatus,
  }
}

function projectDirectFileCertificate(
  value: DirectFileCertificateSnapshot,
): DirectFileCertificateSnapshot {
  return {
    name: value.name,
    activeContentRevision: value.activeContentRevision,
    expiresAt: value.expiresAt,
    lastOutcome: value.lastOutcome,
    lastErrorCode: value.lastErrorCode,
  }
}

function projectCertbotWatcher(value: CertbotWatcherSnapshot): CertbotWatcherSnapshot {
  return {
    health: value.health,
    coalescedEvents: value.coalescedEvents,
    ignoredAccessEvents: value.ignoredAccessEvents,
    backendErrors: value.backendErrors,
    watchRecoveries: value.watchRecoveries,
    watchRefreshes: value.watchRefreshes,
    rescans: value.rescans,
    periodicRescans: value.periodicRescans,
    reconciliationFailures: value.reconciliationFailures,
  }
}

function projectMonitoringRtmp(value: Record<string, unknown>): MonitoringRtmp {
  const accessLog = value.accessLog as MonitoringRtmpAccessLog
  return {
    activeStreams: value.activeStreams as number,
    publishers: value.publishers as number,
    subscribers: value.subscribers as number,
    mediaPayloadBytesReceived: value.mediaPayloadBytesReceived as string,
    recordingSupported: value.recordingSupported as boolean,
    manualRecording: value.manualRecording as boolean,
    recorderBytesWritten: value.recorderBytesWritten as string,
    recorderSegmentsStarted: value.recorderSegmentsStarted as string,
    recorderSegmentsCompleted: value.recorderSegmentsCompleted as string,
    recorderDiscontinuities: value.recorderDiscontinuities as string,
    relayConnectionAttempts: value.relayConnectionAttempts as string,
    relayConnections: value.relayConnections as string,
    relayReconnects: value.relayReconnects as string,
    relayDnsRefreshAttempts: value.relayDnsRefreshAttempts as string,
    relayDnsRefreshSuccesses: value.relayDnsRefreshSuccesses as string,
    relayDnsRefreshFailures: value.relayDnsRefreshFailures as string,
    relayEventsSent: value.relayEventsSent as string,
    relayEventsDropped: value.relayEventsDropped as string,
    relayPayloadBytesSent: value.relayPayloadBytesSent as string,
    accessLog: {
      queueCapacity: accessLog.queueCapacity,
      queueDepth: accessLog.queueDepth,
      enqueued: accessLog.enqueued,
      written: accessLog.written,
      dropped: accessLog.dropped,
      queueSaturated: accessLog.queueSaturated,
      writeFailures: accessLog.writeFailures,
    },
    relays: (value.relays as MonitoringRelay[]).map(projectMonitoringRelay),
    recorders: (value.recorders as MonitoringRecorder[]).map(projectMonitoringRecorder),
  }
}

function projectMonitoringRelay(relay: MonitoringRelay): MonitoringRelay {
  return {
    streamId: relay.streamId,
    relayId: relay.relayId,
    address: relay.address,
    application: relay.application,
    streamName: relay.streamName,
    phase: relay.phase,
    lastFailure: relay.lastFailure,
    queueMessages: relay.queueMessages,
    queueBytes: relay.queueBytes,
    connectionAttempts: relay.connectionAttempts,
    connections: relay.connections,
    reconnects: relay.reconnects,
    dnsRefreshAttempts: relay.dnsRefreshAttempts,
    dnsRefreshSuccesses: relay.dnsRefreshSuccesses,
    dnsRefreshFailures: relay.dnsRefreshFailures,
    lastDnsRefreshFailure: relay.lastDnsRefreshFailure,
    eventsSent: relay.eventsSent,
    eventsDropped: relay.eventsDropped,
    payloadBytesSent: relay.payloadBytesSent,
  }
}

function projectMonitoringRecorder(recorder: MonitoringRecorder): MonitoringRecorder {
  return {
    streamId: recorder.streamId,
    recorderId: recorder.recorderId,
    name: recorder.name,
    manual: recorder.manual,
    phase: recorder.phase,
    bytesWritten: recorder.bytesWritten,
    segmentsStarted: recorder.segmentsStarted,
    segmentsCompleted: recorder.segmentsCompleted,
    discontinuities: recorder.discontinuities,
    currentRelativeName: recorder.currentRelativeName,
    lastCompletedRelativeName: recorder.lastCompletedRelativeName,
    recoverablePartialName: recorder.recoverablePartialName,
    publishedButNotDurableRelativeName: recorder.publishedButNotDurableRelativeName,
  }
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

const MAX_IMPORT_REPORTS = 64
const MAX_IMPORT_REPORT_ITEMS = 4_096
const MAX_IMPORT_REPORT_TEXT_BYTES = 2 * 1024 * 1024

function parseImportReportResponse(value: unknown): ImportReportResponse {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.diskRevision !== 'string' ||
    typeof value.candidateRevision !== 'string' ||
    !(value.activeRevision === null || typeof value.activeRevision === 'string') ||
    !isConfigFormat(value.configFormat) || typeof value.compositional !== 'boolean' ||
    !boundedArray(value.reports, isImportReportSummary, MAX_IMPORT_REPORTS) ||
    !(value.selection === null || isImportReportSelection(value.selection)) ||
    !(value.report === null || isImportReportEnvelope(value.report)) ||
    !(value.preview === null || isImportReportPreview(value.preview)) ||
    !diagnostics(value.diagnostics)
  ) return invalidPayload('native import reports')
  if (value.report !== null && value.selection === null) return invalidPayload('native import reports')
  if (value.report !== null && value.preview !== null && value.report.candidate.config === null) {
    return invalidPayload('native import reports')
  }
  return value as unknown as ImportReportResponse
}

function isImportReportSummary(value: unknown): value is ImportReportSummary {
  return isRecord(value) && safeInteger(value.index) && typeof value.product === 'string' &&
    nullableString(value.version) && nullableString(value.versionSource) &&
    isImportReportCapabilityProfile(value.capabilityProfile) &&
    ['draft', 'finalized', 'blocked'].includes(String(value.status)) &&
    ['rootCount', 'sourceCount', 'dependencyCount', 'blockerCount', 'diagnosticCount',
      'provenanceCount', 'requirementCount', 'overlayCount'].every((key) => safeInteger(value[key])) &&
    typeof value.previewAvailable === 'boolean'
}

function isImportReportSelection(value: unknown): value is { index: number } {
  return isRecord(value) && safeInteger(value.index) && value.index < MAX_IMPORT_REPORTS
}

function isImportReportPreview(value: unknown): value is ImportReportPreview {
  return isRecord(value) && value.format === 'kdl' && boundedText(value.text)
}

function isImportReportEnvelope(value: unknown): value is ImportReportEnvelope {
  return isRecord(value) && value.schemaVersion === 1 && isImportReportSource(value.source) &&
    isImportReportSourceGraph(value.sourceGraph) && isImportReportSourceMetadata(value.sourceMetadata) &&
    isImportReportCandidate(value.candidate) && boundedArray(value.blockers, isImportReportBlocker) &&
    isImportReportRequirements(value.requirements) && boundedArray(value.overlays, isImportReportOverlay) &&
    boundedArray(value.diagnostics, isImportReportDiagnostic) &&
    (value.capabilities === undefined || isRecord(value.capabilities))
}

function isImportReportSource(value: unknown): value is ImportReportSource {
  return isRecord(value) && typeof value.product === 'string' && nullableString(value.version) &&
    nullableString(value.versionSource) && isImportReportCapabilityProfile(value.capabilityProfile)
}

function isImportReportCapabilityProfile(value: unknown): value is ImportReportCapabilityProfile {
  return isRecord(value) && typeof value.id === 'string' && safeInteger(value.version)
}

function isImportReportSourceGraph(value: unknown): value is ImportReportSourceGraph {
  return isRecord(value) && boundedArray(value.roots, isImportReportSourceRoot) &&
    boundedArray(value.sources, isImportReportSourceReference) &&
    boundedArray(value.dependencies, isImportReportDependency) &&
    typeof value.dependenciesComplete === 'boolean' &&
    (value.snapshotStable === null || typeof value.snapshotStable === 'boolean')
}

function isImportReportSourceRoot(value: unknown): value is ImportReportSourceRoot {
  return isRecord(value) && safeInteger(value.ordinal) && nullableString(value.path) &&
    boundedArray(value.sourceIds, safeIntegerValue) && nullableString(value.outcome)
}

function isImportReportSourceReference(value: unknown): value is ImportReportSourceReference {
  return isRecord(value) && safeInteger(value.id) && typeof value.name === 'string' &&
    nullableString(value.path) && safeInteger(value.byteLength) && typeof value.fingerprintSha256 === 'string'
}

function isImportReportDependency(value: unknown): value is ImportReportDependency {
  return isRecord(value) && safeInteger(value.sourceId) &&
    (value.targetSourceId === null || safeInteger(value.targetSourceId)) && typeof value.kind === 'string' &&
    nullableString(value.requestedPath) && nullableString(value.canonicalPath) &&
    (value.optional === null || typeof value.optional === 'boolean') && typeof value.status === 'string' &&
    (value.span === null || isImportReportSpan(value.span)) && nullableString(value.failureCode) &&
    nullableString(value.fingerprintSha256) && typeof value.truncated === 'boolean'
}

function isImportReportSourceMetadata(value: unknown): value is ImportReportSourceMetadata {
  return isRecord(value) && nullableString(value.environmentFingerprintSha256) &&
    boundedArray(value.inactiveSources, isImportReportInactiveSource) &&
    boundedArray(value.originalSourceIds, safeIntegerValue) &&
    boundedArray(value.sourceMaps, isImportReportSourceMap)
}

function isImportReportInactiveSource(value: unknown): value is ImportReportInactiveSource {
  return isRecord(value) && typeof value.condition === 'string' && isImportReportSpan(value.origin)
}

function isImportReportSourceMap(value: unknown): value is ImportReportSourceMap {
  return isRecord(value) && safeInteger(value.sourceId) &&
    boundedArray(value.segments, isImportReportSourceMapSegment)
}

function isImportReportSourceMapSegment(value: unknown): value is ImportReportSourceMapSegment {
  return isRecord(value) && isImportReportRange(value.generated) && isImportReportRange(value.original)
}

function isImportReportRange(value: unknown): value is ImportReportRange {
  return isRecord(value) && safeInteger(value.start) && safeInteger(value.end) && value.end >= value.start
}

function isImportReportSpan(value: unknown): value is ImportReportSpan {
  return isRecord(value) && safeInteger(value.sourceId) && isImportReportRange(value.range)
}

function isImportReportCandidate(value: unknown): value is ImportReportCandidate {
  return isRecord(value) &&
    ((value.finalized === true && isCanonicalConfig(value.config)) ||
      (value.finalized === false && value.config === null)) &&
    isImportReportCandidateDraft(value.draft) && boundedArray(value.provenance, isImportReportProvenance)
}

function isImportReportCandidateDraft(value: unknown): value is ImportReportCandidateDraft {
  return isRecord(value) && safeInteger(value.version) && nullableSafeInteger(value.maxConnections) &&
    typeof value.management === 'boolean' && typeof value.stats === 'boolean' &&
    ['certificates', 'tlsProfiles', 'listeners', 'upstreamPools', 'httpServices', 'cacheStores',
      'forwardProxyServices', 'rtmpServices', 'l4Services'].every((key) => safeInteger(value[key]))
}

function isImportReportProvenance(value: unknown): value is ImportReportProvenance {
  return isRecord(value) && typeof value.path === 'string' && boundedArray(value.origins, isImportReportOrigin)
}

function isImportReportOrigin(value: unknown): value is ImportReportOrigin {
  return isRecord(value) && nullableString(value.role) && safeInteger(value.sourceId) &&
    (value.range === null || isImportReportRange(value.range)) && nullableString(value.path) &&
    nullableSafeInteger(value.line) && boundedArray(value.includeStack, isImportReportSpan)
}

function isImportReportBlocker(value: unknown): value is ImportReportBlocker {
  return isRecord(value) && typeof value.id === 'string' && typeof value.kind === 'string' &&
    typeof value.code === 'string' && typeof value.message === 'string' && nullableString(value.scope) &&
    boundedArray(value.occurrenceIds, (entry): entry is string => typeof entry === 'string') &&
    boundedArray(value.origins, isImportReportSpan)
}

function isImportReportRequirements(value: unknown): value is ImportReportRequirements {
  return isRecord(value) && boundedArray(value.deployment, isImportReportRequirement) &&
    boundedArray(value.activation, isImportReportRequirement)
}

function isImportReportRequirement(value: unknown): value is ImportReportRequirement {
  return isRecord(value) && typeof value.kind === 'string' && typeof value.directive === 'string' &&
    boundedArray(value.values, (entry): entry is string => typeof entry === 'string') &&
    boundedArray(value.origins, isImportReportOrigin) &&
    (value.equivalentRuntimeEndpoint === null || typeof value.equivalentRuntimeEndpoint === 'boolean')
}

function isImportReportOverlay(value: unknown): value is ImportReportOverlay {
  return isRecord(value) && typeof value.id === 'string' && typeof value.kind === 'string' &&
    (value.origin === null || isImportReportOrigin(value.origin)) && typeof value.redactedEvidence === 'boolean' &&
    boundedArray(value.values, (entry): entry is string => typeof entry === 'string') &&
    typeof value.satisfied === 'boolean'
}

function isImportReportDiagnostic(value: unknown): value is ImportReportDiagnostic {
  return isRecord(value) && typeof value.code === 'string' && typeof value.severity === 'string' &&
    typeof value.stage === 'string' && typeof value.message === 'string' &&
    (value.primarySpan === null || isImportReportSpan(value.primarySpan)) &&
    boundedArray(value.includeStack, isImportReportSpan) && boundedArray(value.relatedSpans, isRelatedImportSpan) &&
    nullableString(value.help)
}

function isRelatedImportSpan(value: unknown): value is { span: ImportReportSpan; message: string } {
  return isRecord(value) && isImportReportSpan(value.span) && typeof value.message === 'string'
}

function isConfigFormat(value: unknown): value is ConfigFormat {
  return ['kdl', 'lua', 'uci', 'hocon'].includes(String(value))
}

function boundedArray<T>(value: unknown, predicate: (entry: unknown) => entry is T, limit = MAX_IMPORT_REPORT_ITEMS): value is T[] {
  return Array.isArray(value) && value.length <= limit && value.every(predicate)
}

function safeIntegerValue(value: unknown): value is number {
  return safeInteger(value)
}

function boundedText(value: unknown): value is string {
  return typeof value === 'string' && value.length <= MAX_IMPORT_REPORT_TEXT_BYTES
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
  return projectRecorderSnapshot(value)
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
    rtmpRelayPhase(value.phase) && (value.last_failure === null || rtmpRelayFailure(value.last_failure)) &&
    safeInteger(value.queue_messages) &&
    ['queue_bytes', 'connection_attempts', 'connections', 'reconnects', 'dns_refresh_attempts',
      'dns_refresh_successes', 'dns_refresh_failures', 'events_enqueued',
      'events_sent', 'events_dropped', 'payload_bytes_sent'].every((key) => decimalString(value[key])) &&
    (value.last_dns_refresh_failure === null || rtmpRelayDnsRefreshFailure(value.last_dns_refresh_failure))
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
    nullableString(value.last_completed_relative_name) && nullableString(value.recoverable_partial_name) &&
    (value.last_notification === null || (typeof value.last_notification === 'string' &&
      ['started', 'stopped', 'failed'].includes(value.last_notification)))
}

function recorderPhase(value: unknown): boolean {
  if (!isRecord(value) || typeof value.state !== 'string' ||
    !['idle', 'starting', 'recording', 'stopping', 'failed'].includes(value.state)
  ) {
    return false
  }
  if (value.state === 'idle') {
    return value.operation_id === undefined && value.started_at_unix_ms === undefined && value.code === undefined
  }
  if (typeof value.operation_id !== 'string') return false
  if (value.state === 'recording') {
    return safeInteger(value.started_at_unix_ms) && value.code === undefined
  }
  if (value.state === 'failed') {
    return value.started_at_unix_ms === undefined && typeof value.code === 'string' && [
      'open_failed', 'write_failed', 'close_failed', 'backend_unavailable', 'file_sync_failed',
      'publish_failed', 'directory_sync_failed', 'queue_discontinuity', 'unsupported_codec',
      'shutdown_timed_out', 'worker_panicked', 'stale_publisher',
    ].includes(value.code)
  }
  return value.started_at_unix_ms === undefined && value.code === undefined
}

function monitoringProcess(value: unknown): value is MonitoringProcess {
  return isRecord(value) && safeInteger(value.activeConnections) &&
    typeof value.administrativeState === 'string' &&
    ['ready', 'drain', 'maintenance'].includes(value.administrativeState) &&
    monitoringComponentStatus(value.status) &&
    (value.cpuPercent === null || finiteNumber(value.cpuPercent)) &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    decimalString(value.rejectedConnections) && decimalString(value.retryAttempts) &&
    nullableSafeInteger(value.residentMemoryBytes) && nullableSafeInteger(value.virtualMemoryBytes) &&
    nullableSafeInteger(value.threadCount) && nullableSafeInteger(value.openFileDescriptors)
}

function monitoringHost(value: unknown): value is MonitoringHost {
  return isRecord(value) && monitoringComponentStatus(value.status) &&
    (value.loadAverage1m === null || finiteNumber(value.loadAverage1m)) &&
    (value.loadAverage5m === null || finiteNumber(value.loadAverage5m)) &&
    (value.loadAverage15m === null || finiteNumber(value.loadAverage15m)) &&
    nullableSafeInteger(value.totalMemoryBytes) && nullableSafeInteger(value.availableMemoryBytes)
}

function monitoringComponentStatus(value: unknown): value is MonitoringComponentStatus {
  return isRecord(value) && typeof value.state === 'string' &&
    ['healthy', 'degraded', 'unsupported'].includes(value.state) &&
    (value.reason === undefined || typeof value.reason === 'string')
}

function monitoringTraffic(value: unknown): value is MonitoringTraffic {
  return isRecord(value) && decimalString(value.acceptedConnections) &&
    decimalString(value.rejectedConnections) && safeInteger(value.activeConnections) &&
    decimalString(value.bytesReceived) && decimalString(value.bytesSent)
}

function certbotCertificate(value: unknown): value is CertbotCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' && safeInteger(value.activeArchiveRevision) &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function acmeManagedCertificate(value: unknown): value is AcmeManagedCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' && typeof value.directoryUrl === 'string' &&
    typeof value.diskRevision === 'string' && typeof value.activeRevision === 'string' &&
    typeof value.expiresAt === 'string' && nullableSafeInteger(value.notBeforeUnixSeconds) &&
    nullableSafeInteger(value.notAfterUnixSeconds) && nullableSafeInteger(value.nextActionUnixSeconds) &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode) &&
    typeof value.renewalInformationStatus === 'string' && nullableString(value.dnsProvider) &&
    nullableString(value.dnsProviderDeployment) && nullableString(value.dnsProviderHealth) &&
    typeof value.dnsCleanupStatus === 'string'
}

function directFileCertificate(value: unknown): value is DirectFileCertificateSnapshot {
  return isRecord(value) && typeof value.name === 'string' &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function certbotWatcher(value: unknown): value is CertbotWatcherSnapshot {
  return isRecord(value) && typeof value.health === 'string' &&
    ['healthy', 'degraded', 'stopped'].includes(value.health) &&
    ['coalescedEvents', 'ignoredAccessEvents', 'backendErrors', 'watchRecoveries', 'watchRefreshes',
      'rescans', 'periodicRescans', 'reconciliationFailures'].every((key) => decimalString(value[key]))
}

function monitoringRecorder(value: unknown): boolean {
  return isRecord(value) && typeof value.streamId === 'string' && typeof value.recorderId === 'string' &&
    nullableString(value.name) && typeof value.manual === 'boolean' &&
    typeof value.phase === 'string' &&
    ['idle', 'starting', 'recording', 'stopping', 'failed'].includes(value.phase) &&
    decimalString(value.bytesWritten) && decimalString(value.segmentsStarted) &&
    decimalString(value.segmentsCompleted) && decimalString(value.discontinuities) &&
    nullableString(value.currentRelativeName) && nullableString(value.lastCompletedRelativeName) &&
    nullableString(value.recoverablePartialName) && nullableString(value.publishedButNotDurableRelativeName)
}

function monitoringRelay(value: unknown): boolean {
  return isRecord(value) && typeof value.streamId === 'string' && typeof value.relayId === 'string' &&
    typeof value.address === 'string' && typeof value.application === 'string' &&
    typeof value.streamName === 'string' &&
    rtmpRelayPhase(value.phase) && (value.lastFailure === null || rtmpRelayFailure(value.lastFailure)) &&
    safeInteger(value.queueMessages) &&
    ['queueBytes', 'connectionAttempts', 'connections', 'reconnects', 'dnsRefreshAttempts',
      'dnsRefreshSuccesses', 'dnsRefreshFailures', 'eventsSent', 'eventsDropped',
      'payloadBytesSent'].every((key) => decimalString(value[key])) &&
    (value.lastDnsRefreshFailure === null || rtmpRelayDnsRefreshFailure(value.lastDnsRefreshFailure))
}

function monitoringRtmpAccessLog(value: unknown): value is MonitoringRtmpAccessLog {
  return isRecord(value) && safeInteger(value.queueCapacity) &&
    ['queueDepth', 'enqueued', 'written', 'dropped', 'queueSaturated', 'writeFailures']
      .every((key) => decimalString(value[key]))
}

function monitoringTransportOperation(value: unknown): value is MonitoringTransportOperation {
  return isRecord(value) && monitoringTransport(value.transport) && Array.isArray(value.outcomes) &&
    value.outcomes.every((outcome) => isRecord(outcome) && monitoringTransportOutcome(outcome.outcome) &&
      decimalString(outcome.count)) && monitoringLatency(value.latency)
}

function monitoringAccessRecord(value: unknown): value is MonitoringAccessRecord {
  return isRecord(value) && safeInteger(value.timestampUnixMs) && typeof value.correlationId === 'string' &&
    typeof value.listener === 'string' && monitoringTransport(value.transport) &&
    monitoringTransportOutcome(value.outcome) && decimalString(value.durationMs) &&
    decimalString(value.bytesReceived) && decimalString(value.bytesSent)
}

function monitoringLatency(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.buckets) && value.buckets.every((bucket) =>
    isRecord(bucket) && nullableSafeInteger(bucket.upperBoundMs) && decimalString(bucket.count)) &&
    decimalString(value.count) && decimalString(value.sumMs)
}

function monitoringTransport(value: unknown): value is MonitoringTransport {
  return typeof value === 'string' &&
    ['http', 'rtmp', 'forward', 'cache', 'tcp', 'udp', 'h3', 'acme'].includes(value)
}

function monitoringTransportOutcome(value: unknown): value is MonitoringTransportOutcome {
  return typeof value === 'string' &&
    ['success', 'client_error', 'server_error', 'upstream_error', 'timeout', 'rejected', 'cancelled',
      'internal_error', 'degraded'].includes(value)
}

function rtmpRelayPhase(value: unknown): value is RtmpRelayPhase {
  return typeof value === 'string' && RTMP_RELAY_PHASES.includes(value as RtmpRelayPhase)
}

function rtmpRelayFailure(value: unknown): value is RtmpRelayFailure {
  return typeof value === 'string' && RTMP_RELAY_FAILURES.includes(value as RtmpRelayFailure)
}

function rtmpRelayDnsRefreshFailure(value: unknown): value is RtmpRelayDnsRefreshFailure {
  return typeof value === 'string' &&
    RTMP_RELAY_DNS_REFRESH_FAILURES.includes(value as RtmpRelayDnsRefreshFailure)
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
