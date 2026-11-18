import type {
  AdministrativeState, HealthFailure, HealthOverride, MonitoringListenerProtocol, MonitoringPool,
} from '../api'

export const healthFailureLabels: Record<HealthFailure, string> = {
  timeout: 'Probe timed out',
  connect_failed: 'Connection failed',
  unexpected_status: 'Unexpected status',
  protocol_error: 'Protocol error',
  slow: 'Slower than its peers',
}

export const administrativeStateLabels: Record<AdministrativeState, string> = {
  ready: 'Ready',
  drain: 'Drain',
  maintenance: 'Maintenance',
}

export const healthOverrideLabels: Record<HealthOverride, string> = {
  auto: 'Automatic',
  up: 'Forced up',
  down: 'Forced down',
}

export const poolAlgorithmLabels: Record<MonitoringPool['algorithm'], string> = {
  round_robin: 'Round robin',
  weighted_round_robin: 'Weighted round robin',
  least_connections: 'Least connections',
  power_of_two_choices: 'Power of two choices',
  first: 'First server',
}

export const listenerProtocolLabels: Record<MonitoringListenerProtocol, string> = {
  http: 'HTTP',
  tcp: 'TCP',
  rtmp: 'RTMP',
  http3: 'HTTP/3',
  udp: 'UDP',
  forward_http1: 'Forward H1',
  forward_http2: 'Forward H2',
  forward_http3: 'Forward H3',
}
