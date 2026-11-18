import type { TopologyEdgeKind, TopologyNodeKind } from '../api'

export const kindLabels: Record<TopologyNodeKind, string> = {
  listener: 'Listener',
  forward_proxy_listener: 'Forward proxy listener',
  forward_proxy_service: 'Forward proxy service',
  rtmp_listener: 'RTMP listener',
  tls_profile: 'TLS profile',
  certificate: 'Certificate',
  http_service: 'HTTP service',
  http_route: 'HTTP route',
  l4_service: 'L4 service',
  upstream_pool: 'Upstream pool',
  endpoint: 'Endpoint',
}

export const edgeLabels: Record<TopologyEdgeKind, string> = {
  dispatch_service: 'Dispatches to service',
  service_route: 'Contains route',
  route_pool: 'Routes to pool',
  service_pool: 'Uses pool',
  pool_endpoint: 'Contains endpoint',
  listener_tls: 'Uses TLS profile',
  tls_certificate: 'Uses certificate',
}
