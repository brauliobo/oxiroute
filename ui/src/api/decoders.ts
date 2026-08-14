import { decimalString, isRecord, nullableSafeInteger, safeInteger } from '../valueGuards'
import type {
  ListenerInventoryResponse,
  MonitoringListener,
  MonitoringPool,
  MonitoringPoolEndpoint,
  PoolInventoryResponse,
  RuntimeStatus,
  ServerInventoryEntry,
  ServerInventoryResponse,
} from './managementContracts'

export function parseRuntimeStatus(value: unknown): RuntimeStatus {
  if (!isRecord(value) || value.schemaVersion !== 1 || typeof value.buildVersion !== 'string' ||
    !nullableRevision(value.diskRevision) || !nullableRevision(value.candidateRevision) ||
    !nullableRevision(value.activeRevision) || !nullableRevision(value.previousRevision) ||
    typeof value.degraded !== 'boolean' || !Array.isArray(value.listeners) ||
    !value.listeners.every(monitoringListener)
  ) return invalidPayload('runtime status')
  return value as unknown as RuntimeStatus
}

export function parseListenerInventory(value: unknown): ListenerInventoryResponse {
  if (!isRecord(value) || !Array.isArray(value.listeners) || !value.listeners.every(monitoringListener)) {
    return invalidPayload('listener inventory')
  }
  return value as unknown as ListenerInventoryResponse
}

export function parsePoolInventory(value: unknown): PoolInventoryResponse {
  if (!isRecord(value) || !Array.isArray(value.pools) || !value.pools.every(monitoringPool)) {
    return invalidPayload('pool inventory')
  }
  return value as unknown as PoolInventoryResponse
}

export function parseServerInventory(value: unknown): ServerInventoryResponse {
  if (!isRecord(value) || !Array.isArray(value.servers) || !value.servers.every(serverInventoryEntry)) {
    return invalidPayload('server inventory')
  }
  return value as unknown as ServerInventoryResponse
}

export function monitoringListener(value: unknown): value is MonitoringListener {
  return monitoringTraffic(value) && isRecord(value) && typeof value.name === 'string' &&
    typeof value.protocol === 'string' &&
    ['http', 'tcp', 'rtmp', 'http3', 'udp', 'forward_http1', 'forward_http2', 'forward_http3'].includes(value.protocol) &&
    typeof value.bind === 'string' &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    typeof value.state === 'string' && ['configured', 'listening', 'stopped', 'failed'].includes(value.state) &&
    (value.httpOperations === null || monitoringHttpOperations(value.httpOperations)) &&
    (value.tcpRelays === null || monitoringTcpRelays(value.tcpRelays)) &&
    (value.proxyProtocol === null || monitoringProxyProtocol(value.proxyProtocol)) &&
    (value.cache === null || monitoringCache(value.cache))
}

export function monitoringPool(value: unknown): value is MonitoringPool {
  return isRecord(value) && typeof value.name === 'string' &&
    typeof value.algorithm === 'string' &&
    ['first', 'round_robin', 'least_connections', 'weighted_round_robin'].includes(value.algorithm) &&
    safeInteger(value.availableEndpoints) && safeInteger(value.totalEndpoints) &&
    decimalString(value.unavailableSelections) && safeInteger(value.queued) &&
    decimalString(value.queuedTotal) && decimalString(value.queueTimeouts) &&
    decimalString(value.queueCancellations) && Array.isArray(value.endpoints) &&
    value.endpoints.every(monitoringPoolEndpoint)
}

export function monitoringPoolEndpoint(value: unknown): value is MonitoringPoolEndpoint {
  return isRecord(value) && typeof value.address === 'string' && typeof value.name === 'string' &&
    decimalString(value.activeConnections) && typeof value.administrativeState === 'string' &&
    ['ready', 'drain', 'maintenance'].includes(value.administrativeState) &&
    typeof value.checksEnabled === 'boolean' && typeof value.checksRunning === 'boolean' &&
    (value.configuredMaxConnections === null || safeInteger(value.configuredMaxConnections)) &&
    typeof value.healthOverride === 'string' && ['auto', 'up', 'down'].includes(value.healthOverride) &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    typeof value.state === 'string' && ['unchecked', 'unknown', 'healthy', 'unhealthy'].includes(value.state) &&
    nullableSafeInteger(value.lastCheckedAtUnixMs) && nullableSafeInteger(value.lastTransitionAtUnixMs) &&
    decimalString(value.successfulChecks) && decimalString(value.failedChecks) &&
    decimalString(value.consecutiveSuccesses) && decimalString(value.consecutiveFailures) &&
    (value.lastFailure === null || (typeof value.lastFailure === 'string' &&
      ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'].includes(value.lastFailure))) &&
    safeInteger(value.weight) && typeof value.passiveEjected === 'boolean' &&
    decimalString(value.passiveFailureCount) && decimalString(value.passiveConsecutiveFailures) &&
    decimalString(value.passiveEjectionCount) &&
    (value.passiveEjectionReason === null || (typeof value.passiveEjectionReason === 'string' &&
      ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'].includes(value.passiveEjectionReason))) &&
    nullableSafeInteger(value.passiveEjectedAtUnixMs) && nullableSafeInteger(value.passiveEjectionUntilUnixMs) &&
    decimalString(value.passiveRecoveryCount) && nullableSafeInteger(value.passiveLastRecoveryAtUnixMs)
}

function serverInventoryEntry(value: unknown): value is ServerInventoryEntry {
  return isRecord(value) && typeof value.pool === 'string' && monitoringPoolEndpoint(value.server)
}

function monitoringTraffic(value: unknown): boolean {
  return isRecord(value) && decimalString(value.acceptedConnections) &&
    decimalString(value.rejectedConnections) && safeInteger(value.activeConnections) &&
    decimalString(value.bytesReceived) && decimalString(value.bytesSent)
}

function monitoringHttpOperations(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.outcomes) && value.outcomes.every((outcome) =>
    isRecord(outcome) && typeof outcome.result === 'string' &&
    ['success', 'client_error', 'server_error', 'upstream_error', 'timeout', 'cancelled', 'internal_error']
      .includes(outcome.result) && decimalString(outcome.count)) && monitoringLatency(value.latency)
}

function monitoringTcpRelays(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.outcomes) && value.outcomes.every((outcome) =>
    isRecord(outcome) && typeof outcome.result === 'string' &&
    ['success', 'connect_error', 'connect_timeout', 'idle_timeout', 'lifetime_timeout', 'cancelled',
      'io_error', 'accounting_error', 'proxy_protocol_error'].includes(outcome.result) &&
    decimalString(outcome.count)) && monitoringLatency(value.latency)
}

function monitoringProxyProtocol(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.outcomes) && value.outcomes.every((outcome) =>
    isRecord(outcome) && typeof outcome.result === 'string' &&
    ['accepted', 'sent', 'timeout', 'cancelled', 'malformed', 'unsupported', 'mismatch', 'io_error']
      .includes(outcome.result) && decimalString(outcome.count))
}

function monitoringLatency(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.buckets) && value.buckets.every((bucket) =>
    isRecord(bucket) && nullableSafeInteger(bucket.upperBoundMs) && decimalString(bucket.count)) &&
    decimalString(value.count) && decimalString(value.sumMs)
}

function monitoringCache(value: unknown): boolean {
  return isRecord(value) && ['hits', 'misses', 'admissions', 'evictions']
    .every((key) => decimalString(value[key]))
}

function nullableRevision(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
