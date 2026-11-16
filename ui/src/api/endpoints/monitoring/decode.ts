import {
  decimalString,
  finiteNumber,
  isRecord,
  nullableSafeInteger,
  nullableString,
  safeInteger,
} from '../../../valueGuards'
import { monitoringListener, monitoringPool } from '../../decoders'
import type {
  MonitoringCache,
  MonitoringHttpOperations,
  MonitoringListener,
  MonitoringPool,
  MonitoringPoolEndpoint,
  MonitoringProxyProtocol,
  MonitoringTcpRelays,
  MonitoringTraffic,
} from '../../managementContracts'
import { rtmpRelayDnsRefreshFailure, rtmpRelayFailure, rtmpRelayPhase } from '../rtmp/decode'
import { invalidPayload } from '../shared'
import {
  acmeManagedCertificate,
  certbotCertificate,
  certbotWatcher,
  directFileCertificate,
  projectAcmeManagedCertificate,
  projectCertbotCertificate,
  projectCertbotWatcher,
  projectDirectFileCertificate,
} from '../tls/decode'
import type {
  MonitoringAccessRecord,
  MonitoringComponentStatus,
  MonitoringHost,
  MonitoringProcess,
  MonitoringRecorder,
  MonitoringRelay,
  MonitoringRtmp,
  MonitoringRtmpAccessLog,
  MonitoringSnapshot,
  MonitoringTransport,
  MonitoringTransportOperation,
  MonitoringTransportOutcome,
} from './types'

export function parseMonitoring(value: unknown): MonitoringSnapshot {
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
    failOpenSelections: value.failOpenSelections,
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
    origin: value.origin,
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
