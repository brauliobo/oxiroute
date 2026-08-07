import type { CanonicalConfig } from '../config'

const TLS_ALPN_BIND = '0.0.0.0:443'
const TLS_ALPN_PROTOCOLS = ['http', 'forward_http1', 'forward_http2'] as const

export type TlsAlpnDeployment =
  | {
      outcome: 'prepared'
      listenerIndex: number
      listenerName: string
      profileName: string
      serviceName: string
      bindAddress: string
    }
  | {
      outcome: 'ready'
      listenerName: string
      profileName: string
      bindAddress: string
    }
  | {
      outcome: 'blocked'
      message: string
    }

export function prepareTlsAlpnDeployment(
  config: CanonicalConfig,
  certificateName: string,
): TlsAlpnDeployment {
  const certificate = config.certificates.find(({ name }) => name === certificateName)
  if (!certificate || certificate.source.type !== 'acme_managed' || certificate.source.challenge !== 'tls_alpn01') {
    return blocked('Select a managed ACME certificate using TLS-ALPN-01 before preparing a listener.')
  }

  const portListeners = config.listeners.filter((listener) =>
    listener.bind.type === 'socket' && socketPort(listener.bind.address) === 443,
  )
  if (portListeners.length > 1) {
    return blocked('The draft contains more than one TCP listener on port 443. Resolve that bind conflict before preparing TLS-ALPN.')
  }

  const existing = portListeners[0]
  if (existing) {
    const bindAddress = existing.bind.type === 'socket' ? existing.bind.address : 'TCP port 443'
    if (existing.tls_profile === null) {
      return blocked(`Listener ${existing.name} already owns ${bindAddress} without TLS. Free the bind or assign a compatible TLS profile before preparing TLS-ALPN.`)
    }
    if (!TLS_ALPN_PROTOCOLS.includes(existing.protocol as typeof TLS_ALPN_PROTOCOLS[number])) {
      return blocked(`Listener ${existing.name} on ${bindAddress} does not terminate supported HTTP TLS traffic.`)
    }
    const profile = config.tls_profiles.find(({ name }) => name === existing.tls_profile)
    if (!profile) {
      return blocked(`Listener ${existing.name} references TLS profile ${existing.tls_profile}, which is not present in the draft.`)
    }
    if (profile.policy.client_auth.mode === 'required') {
      return blocked(`TLS profile ${profile.name} requires client certificates, which ACME TLS-ALPN-01 cannot provide.`)
    }
    return {
      outcome: 'ready',
      listenerName: existing.name,
      profileName: profile.name,
      bindAddress,
    }
  }

  const baseName = deploymentBaseName(certificateName)
  const serviceName = uniqueName(config.http_services.map(({ name }) => name), `${baseName}-service`)
  const profileName = uniqueName(config.tls_profiles.map(({ name }) => name), `${baseName}-profile`)
  const listenerName = uniqueName(config.listeners.map(({ name }) => name), `${baseName}-listener`)

  const route = {
    host: null,
    path: { kind: 'segment_prefix' as const, value: '/' },
    methods: [],
    access_policy: null,
    policy: {
      max_request_body_bytes: 10_485_760,
      connect_timeout_ms: 30_000,
      read_timeout_ms: 30_000,
      write_timeout_ms: 30_000,
      request_buffering: false,
      response_buffering: false,
    },
    action: { type: 'fixed_response' as const, status: 404, body: '', headers: [] },
  }

  config.http_services.push({
    name: serviceName,
    routes: [route],
    automatic_response_headers: true,
    upstream_io_timeout_ms: 30_000,
    max_request_body_bytes: 10_485_760,
    gzip: null,
    access_log: null,
  })
  config.tls_profiles.push({
    name: profileName,
    certificates: [certificateName],
    default_certificate: certificateName,
    min_version: '1.2',
    alpn: ['h2', 'http/1.1'],
    policy: {
      cipher_list: null,
      dh_parameters_path: null,
      client_auth: {
        mode: 'disabled',
        ca_certificate_path: null,
        allowed_dns_names: [],
      },
      session_cache: null,
      session_timeout_seconds: null,
      session_tickets: false,
      prefer_server_ciphers: true,
    },
  })
  config.listeners.push({
    name: listenerName,
    bind: { type: 'socket', address: TLS_ALPN_BIND },
    protocol: 'http',
    service: serviceName,
    tls_profile: profileName,
    max_connections: 10_000,
    downstream_timeouts: {
      client_timeout_ms: null,
      request_timeout_ms: null,
      keepalive_timeout_ms: null,
    },
  })

  return {
    outcome: 'prepared',
    listenerIndex: config.listeners.length - 1,
    listenerName,
    profileName,
    serviceName,
    bindAddress: TLS_ALPN_BIND,
  }
}

function blocked(message: string): TlsAlpnDeployment {
  return { outcome: 'blocked', message }
}

function socketPort(address: string): number | null {
  const match = /:(\d+)$/.exec(address.trim())
  if (!match) return null
  const port = Number(match[1])
  return Number.isInteger(port) ? port : null
}

function deploymentBaseName(certificateName: string): string {
  const slug = certificateName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40)
  return `${slug || 'managed-certificate'}-tls-alpn`
}

function uniqueName(existing: string[], base: string): string {
  const names = new Set(existing)
  if (!names.has(base)) return base
  let suffix = 2
  while (names.has(`${base}-${suffix}`)) suffix += 1
  return `${base}-${suffix}`
}
