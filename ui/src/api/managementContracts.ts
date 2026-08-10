import type { ListenerProtocol, UpstreamAlgorithm } from '../config'

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
