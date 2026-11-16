import type { HttpRetryTrigger, ListenerBind, UpstreamEndpoint } from '../../../config'
import type { EndpointHealthState, HealthFailure, ListenerRuntimeState } from '../../managementContracts'

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
