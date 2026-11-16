import { decimalString, isRecord, nullableSafeInteger, safeInteger } from '../../../valueGuards'
import { invalidPayload } from '../shared'
import type { CandidateTopologySnapshot, TopologySnapshot } from './types'

export function parseTopology(value: unknown): TopologySnapshot {
  if (!isRecord(value) || value.schemaVersion !== 1 || !isRecord(value.state) ||
    value.state.config !== 'active' || !['active', 'starting', 'degraded'].includes(String(value.state.runtime)) ||
    !safeInteger(value.state.sampledAtUnixMs) || !Array.isArray(value.nodes) ||
    !value.nodes.every(topologyNode) || !Array.isArray(value.edges) ||
    !value.edges.every(topologyEdge) || !Array.isArray(value.overlays) ||
    !value.overlays.every(topologyOverlay)
  ) return invalidPayload('topology')
  return value as unknown as TopologySnapshot
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
      ['timeout', 'connect_failed', 'unexpected_status', 'protocol_error', 'slow'].includes(String(value.lastFailure)))
}

export function candidateTopology(value: unknown): value is CandidateTopologySnapshot {
  return isRecord(value) && value.schemaVersion === 1 && isRecord(value.state) &&
    value.state.config === 'candidate' && value.state.runtime === 'not_active' &&
    safeInteger(value.state.sampledAtUnixMs) && Array.isArray(value.nodes) &&
    value.nodes.every(topologyNode) && Array.isArray(value.edges) && value.edges.every(topologyEdge) &&
    Array.isArray(value.overlays) && value.overlays.length === 0
}
