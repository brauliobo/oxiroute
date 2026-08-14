import type { UpstreamAlgorithm } from '../config'
import type {
  AdministrativeStateDto,
  CacheDto,
  EndpointDto,
  EndpointHealthStateDto,
  HealthFailureDto,
  HealthOverrideDto,
  HttpOperationDto,
  LatencyDto,
  ListenerDto,
  ListenerInventoryResponse as GeneratedListenerInventoryResponse,
  ListenerRuntimeStateDto,
  PoolDto,
  PoolInventoryResponse as GeneratedPoolInventoryResponse,
  ProxyProtocolDto,
  ServerInventoryEntry as GeneratedServerInventoryEntry,
  ServerInventoryResponse as GeneratedServerInventoryResponse,
  StatusResponse,
  TcpRelayDto,
} from './generated/controlPlane'

export type MonitoringTraffic = Pick<ListenerDto, 'acceptedConnections' | 'rejectedConnections' | 'activeConnections' | 'bytesReceived' | 'bytesSent'>

export type ListenerRuntimeState = ListenerRuntimeStateDto
export type AdministrativeState = AdministrativeStateDto
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

export type MonitoringLatency = LatencyDto

export type MonitoringHttpOperations = HttpOperationDto

export type MonitoringTcpRelays = TcpRelayDto

export type MonitoringProxyProtocol = ProxyProtocolDto

export type MonitoringCache = CacheDto

export type MonitoringListener = Omit<ListenerDto, 'administrativeState' | 'protocol' | 'state' | 'httpOperations' | 'tcpRelays' | 'proxyProtocol' | 'cache'> & {
  administrativeState: AdministrativeState
  protocol: MonitoringListenerProtocol
  state: ListenerRuntimeState
  httpOperations: MonitoringHttpOperations | null
  tcpRelays: MonitoringTcpRelays | null
  proxyProtocol: MonitoringProxyProtocol | null
  cache: MonitoringCache | null
}

export type EndpointHealthState = EndpointHealthStateDto
export type HealthFailure = HealthFailureDto
export type HealthOverride = HealthOverrideDto
export type MonitoringUpstreamAlgorithm = Extract<UpstreamAlgorithm, string> | 'weighted_round_robin'

export type MonitoringPoolEndpoint = Omit<EndpointDto, 'administrativeState' | 'healthOverride' | 'state' | 'lastFailure' | 'passiveEjectionReason'> & {
  administrativeState: AdministrativeState
  healthOverride: HealthOverride
  state: EndpointHealthState
  lastFailure: HealthFailure | null
  passiveEjectionReason: HealthFailure | null
}

export type MonitoringPool = Omit<PoolDto, 'algorithm' | 'endpoints'> & {
  algorithm: MonitoringUpstreamAlgorithm
  endpoints: MonitoringPoolEndpoint[]
}

export type RuntimeStatus = Omit<StatusResponse, 'listeners'> & { listeners: MonitoringListener[] }

export type ListenerInventoryResponse = Omit<GeneratedListenerInventoryResponse, 'listeners'> & {
  listeners: MonitoringListener[]
}

export type PoolInventoryResponse = Omit<GeneratedPoolInventoryResponse, 'pools'> & { pools: MonitoringPool[] }

export type ServerInventoryEntry = Omit<GeneratedServerInventoryEntry, 'server'> & {
  server: MonitoringPoolEndpoint
}

export type ServerInventoryResponse = Omit<GeneratedServerInventoryResponse, 'servers'> & {
  servers: ServerInventoryEntry[]
}

export interface ServerTarget {
  pool: string
  server: string
}
