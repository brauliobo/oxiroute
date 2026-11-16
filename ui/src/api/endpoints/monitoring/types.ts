import type {
  AdministrativeState,
  MonitoringListener,
  MonitoringPool,
  MonitoringTraffic,
} from '../../managementContracts'
import type { RecorderPhase, RelaySnapshot, RtmpRelayDnsRefreshFailure } from '../rtmp/types'
import type {
  AcmeManagedCertificateSnapshot,
  CertbotCertificateSnapshot,
  CertbotWatcherSnapshot,
  DirectFileCertificateSnapshot,
  DirectFileWatcherSnapshot,
} from '../tls/types'

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
