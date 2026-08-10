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
} from './endpoints'

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
    ['http', 'tcp', 'rtmp', 'forward_http1', 'forward_http2', 'forward_http3']
      .includes(String(value.protocol)) && typeof value.bind === 'string' &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    ['configured', 'listening', 'stopped', 'failed'].includes(String(value.state))
}

export function monitoringPool(value: unknown): value is MonitoringPool {
  return isRecord(value) && typeof value.name === 'string' &&
    ['first', 'round_robin', 'least_connections', 'weighted_round_robin'].includes(String(value.algorithm)) &&
    safeInteger(value.availableEndpoints) && safeInteger(value.totalEndpoints) &&
    decimalString(value.unavailableSelections) && safeInteger(value.queued) &&
    decimalString(value.queuedTotal) && decimalString(value.queueTimeouts) &&
    decimalString(value.queueCancellations) && Array.isArray(value.endpoints) &&
    value.endpoints.every(monitoringPoolEndpoint)
}

export function monitoringPoolEndpoint(value: unknown): value is MonitoringPoolEndpoint {
  return isRecord(value) && typeof value.address === 'string' && typeof value.name === 'string' &&
    decimalString(value.activeConnections) && ['ready', 'drain', 'maintenance'].includes(String(value.administrativeState)) &&
    typeof value.checksEnabled === 'boolean' && typeof value.checksRunning === 'boolean' &&
    (value.configuredMaxConnections === null || safeInteger(value.configuredMaxConnections)) &&
    ['auto', 'up', 'down'].includes(String(value.healthOverride)) &&
    (value.maxConnections === null || safeInteger(value.maxConnections)) &&
    ['unchecked', 'unknown', 'healthy', 'unhealthy'].includes(String(value.state)) &&
    nullableSafeInteger(value.lastCheckedAtUnixMs) && nullableSafeInteger(value.lastTransitionAtUnixMs) &&
    decimalString(value.successfulChecks) && decimalString(value.failedChecks) &&
    decimalString(value.consecutiveSuccesses) && decimalString(value.consecutiveFailures) &&
    (value.lastFailure === null || ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error'].includes(String(value.lastFailure)))
}

function serverInventoryEntry(value: unknown): value is ServerInventoryEntry {
  return isRecord(value) && typeof value.pool === 'string' && monitoringPoolEndpoint(value.server)
}

function monitoringTraffic(value: unknown): boolean {
  return isRecord(value) && decimalString(value.acceptedConnections) &&
    decimalString(value.rejectedConnections) && safeInteger(value.activeConnections) &&
    decimalString(value.bytesReceived) && decimalString(value.bytesSent)
}

function nullableRevision(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
