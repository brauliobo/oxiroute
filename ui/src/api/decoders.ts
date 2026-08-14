import { decimalString, integerInRange, isRecord, nullableSafeInteger, nullableString, safeInteger } from '../valueGuards'
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
    typeof value.degraded !== 'boolean' || !safeInteger(value.activeGenerationAgeMs) ||
    !statusComponents(value.components) || !statusCertificates(value.certificates) ||
    !auditStatus(value.audit) || !statusCapabilities(value.capabilities) ||
    !Array.isArray(value.listeners) || !value.listeners.every(monitoringListener) ||
    !Array.isArray(value.tlsProfiles) || !value.tlsProfiles.every(statusTlsProfile)
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

function statusComponents(value: unknown): boolean {
  return isRecord(value) && componentStatus(value.process) && componentStatus(value.host) &&
    componentStatus(value.generation) && auditStatus(value.audit)
}

function componentStatus(value: unknown): boolean {
  return isRecord(value) && enumValue(value.state, ['healthy', 'degraded', 'unsupported']) &&
    (value.reason === undefined || nullableString(value.reason))
}

function statusCertificates(value: unknown): boolean {
  return isRecord(value) && Array.isArray(value.certbot) && value.certbot.every(certbotCertificate) &&
    Array.isArray(value.acmeManaged) && value.acmeManaged.every(acmeManagedCertificate) &&
    Array.isArray(value.directFiles) && value.directFiles.every(directFileCertificate)
}

function certbotCertificate(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' &&
    safeInteger(value.activeArchiveRevision) && typeof value.activeContentRevision === 'string' &&
    typeof value.expiresAt === 'string' && nullableString(value.lastOutcome) &&
    nullableString(value.lastErrorCode)
}

function acmeManagedCertificate(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' && typeof value.directoryUrl === 'string' &&
    typeof value.diskRevision === 'string' && typeof value.activeRevision === 'string' &&
    typeof value.expiresAt === 'string' && nullableSafeInteger(value.notBeforeUnixSeconds) &&
    nullableSafeInteger(value.notAfterUnixSeconds) && nullableSafeInteger(value.nextActionUnixSeconds) &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode) &&
    nullableString(value.dnsProvider) && nullableString(value.dnsProviderDeployment) &&
    nullableString(value.dnsProviderHealth) && typeof value.renewalInformationStatus === 'string' &&
    typeof value.dnsCleanupStatus === 'string'
}

function directFileCertificate(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' &&
    typeof value.activeContentRevision === 'string' && typeof value.expiresAt === 'string' &&
    nullableString(value.lastOutcome) && nullableString(value.lastErrorCode)
}

function auditStatus(value: unknown): boolean {
  return isRecord(value) && enumValue(value.state, ['healthy', 'degraded', 'memory']) &&
    typeof value.persistent === 'boolean' && typeof value.degraded === 'boolean' &&
    safeInteger(value.recordCount) && safeInteger(value.bytes) && safeInteger(value.rotatedFiles) &&
    safeInteger(value.maxRecords) && safeInteger(value.maxRecordBytes) &&
    safeInteger(value.maxFileBytes) && safeInteger(value.maxTotalBytes) &&
    safeInteger(value.maxRotatedFiles) && safeInteger(value.writeFailures) &&
    safeInteger(value.corruptRecords) &&
    (value.lastError === undefined || nullableString(value.lastError))
}

function statusCapabilities(value: unknown): boolean {
  return isRecord(value) && integerInRange(value.schemaVersion, 0, 255) && supervisionCapability(value.supervision) &&
    udpCapability(value.udp) && isRecord(value.http3) && h3Capability(value.http3.forward) &&
    h3Capability(value.http3.reverse)
}

function supervisionCapability(value: unknown): boolean {
  return isRecord(value) && enumValue(value.mode, ['direct', 'supervised']) &&
    isRecord(value.descriptorAdoption) &&
    enumValue(value.descriptorAdoption.status, ['negotiated', 'not_used']) &&
    integerInRange(value.descriptorAdoption.manifestVersion, 0, 255) &&
    typeof value.descriptorAdoption.datagram === 'boolean' &&
    typeof value.descriptorAdoption.quic === 'boolean'
}

function udpCapability(value: unknown): boolean {
  return isRecord(value) && enumValue(value.status, ['active', 'unconfigured', 'blocked']) &&
    typeof value.supported === 'boolean' && stringArray(value.listeners) &&
    stringArray(value.configuredListeners) && value.transport === 'udp' &&
    value.drain === 'graceful' && value.fallback === 'none' && blockedReason(value.blockedReason)
}

function h3Capability(value: unknown): boolean {
  return isRecord(value) && enumValue(value.status, ['active', 'unconfigured', 'blocked']) &&
    typeof value.supported === 'boolean' && stringArray(value.listeners) &&
    stringArray(value.configuredListeners) && value.transport === 'quic' &&
    stringArray(value.alpn) && typeof value.tlsMinVersion === 'string' &&
    value.zeroRtt === 'disabled' && value.migration === 'disabled' &&
    value.goAway === 'graceful' && value.fallback === 'none' && stringArray(value.unsupported) &&
    h3Limits(value.limits) && blockedReason(value.blockedReason)
}

function h3Limits(value: unknown): boolean {
  return isRecord(value) && safeInteger(value.maxHandshakesAndConnections) &&
    safeInteger(value.maxBidirectionalStreams) && safeInteger(value.maxUnidirectionalStreams) &&
    safeInteger(value.maxFieldSectionBytes) && safeInteger(value.maxRequestBodyBytes) &&
    safeInteger(value.maxResponseBodyBytes)
}

function blockedReason(value: unknown): boolean {
  return value === null || enumValue(value, ['listener_runtime_failed', 'listener_stopped', 'listener_not_listening'])
}

function statusTlsProfile(value: unknown): boolean {
  return isRecord(value) && typeof value.name === 'string' && isRecord(value.clientAuth) &&
    typeof value.clientAuth.mode === 'string' && typeof value.clientAuth.caConfigured === 'boolean' &&
    safeInteger(value.clientAuth.allowedDnsNameCount)
}

function stringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string')
}

function enumValue(value: unknown, values: readonly string[]): value is string {
  return typeof value === 'string' && values.includes(value)
}

function nullableRevision(value: unknown): value is string | null {
  return value === null || typeof value === 'string'
}

function invalidPayload(source: string): never {
  throw new Error(`The ${source} API returned an invalid response payload.`)
}
