import type { UpstreamAlgorithm } from '../config'

export interface MonitoringTraffic {
  acceptedConnections: string
  rejectedConnections: string
  activeConnections: number
  bytesReceived: string
  bytesSent: string
}

export type ListenerRuntimeState = 'configured' | 'listening' | 'stopped' | 'failed'
export type AdministrativeState = 'ready' | 'drain' | 'maintenance'
export type MonitoringListenerProtocol =
  | 'http'
  | 'tcp'
  | 'rtmp'
  | 'http3'
  | 'udp'
  | 'forward_http1'
  | 'forward_http2'
  | 'forward_http3'
export type MonitoringHttpOperationResult =
  | 'success'
  | 'client_error'
  | 'server_error'
  | 'upstream_error'
  | 'timeout'
  | 'cancelled'
  | 'internal_error'
export type MonitoringTcpRelayResult =
  | 'success'
  | 'connect_error'
  | 'connect_timeout'
  | 'idle_timeout'
  | 'lifetime_timeout'
  | 'cancelled'
  | 'io_error'
  | 'accounting_error'
  | 'proxy_protocol_error'
export type MonitoringProxyProtocolResult =
  | 'accepted'
  | 'sent'
  | 'timeout'
  | 'cancelled'
  | 'malformed'
  | 'unsupported'
  | 'mismatch'
  | 'io_error'

export interface MonitoringLatency {
  buckets: Array<{ upperBoundMs: number | null; count: string }>
  count: string
  sumMs: string
}

export interface MonitoringHttpOperations {
  outcomes: Array<{ result: MonitoringHttpOperationResult; count: string }>
  latency: MonitoringLatency
}

export interface MonitoringTcpRelays {
  outcomes: Array<{ result: MonitoringTcpRelayResult; count: string }>
  latency: MonitoringLatency
}

export interface MonitoringProxyProtocol {
  outcomes: Array<{ result: MonitoringProxyProtocolResult; count: string }>
}

export interface MonitoringCache {
  hits: string
  misses: string
  admissions: string
  evictions: string
}

export interface MonitoringListener extends MonitoringTraffic {
  administrativeState: AdministrativeState
  name: string
  protocol: MonitoringListenerProtocol
  bind: string
  maxConnections: number | null
  state: ListenerRuntimeState
  httpOperations: MonitoringHttpOperations | null
  tcpRelays: MonitoringTcpRelays | null
  proxyProtocol: MonitoringProxyProtocol | null
  cache: MonitoringCache | null
}

export type EndpointHealthState = 'unchecked' | 'unknown' | 'healthy' | 'unhealthy'
export type HealthFailure = 'timeout' | 'connect_failed' | 'unexpected_status' | 'protocol_error'
export type HealthOverride = 'auto' | 'up' | 'down'
export type MonitoringUpstreamAlgorithm = Extract<UpstreamAlgorithm, string> | 'weighted_round_robin'

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
  weight: number
  passiveEjected: boolean
  passiveFailureCount: string
  passiveConsecutiveFailures: string
  passiveEjectionCount: string
  passiveEjectionReason: HealthFailure | null
  passiveEjectedAtUnixMs: number | null
  passiveEjectionUntilUnixMs: number | null
  passiveRecoveryCount: string
  passiveLastRecoveryAtUnixMs: number | null
}

export interface MonitoringPool {
  name: string
  algorithm: MonitoringUpstreamAlgorithm
  availableEndpoints: number
  totalEndpoints: number
  unavailableSelections: string
  queued: number
  queuedTotal: string
  queueTimeouts: string
  queueCancellations: string
  endpoints: MonitoringPoolEndpoint[]
}

export interface RuntimeStatus {
  schemaVersion: 1
  buildVersion: string
  diskRevision: string | null
  candidateRevision: string | null
  activeRevision: string | null
  previousRevision: string | null
  degraded: boolean
  listeners: MonitoringListener[]
}

export interface ListenerInventoryResponse {
  listeners: MonitoringListener[]
}

export interface PoolInventoryResponse {
  pools: MonitoringPool[]
}

export interface ServerTarget {
  pool: string
  server: string
}

export interface ServerInventoryEntry {
  pool: string
  server: MonitoringPoolEndpoint
}

export interface ServerInventoryResponse {
  servers: ServerInventoryEntry[]
}
