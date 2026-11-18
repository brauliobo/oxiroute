import { flushPromises, mount, type VaporWrapper } from './mount'
import { afterEach, vi } from 'vitest'

import ConfigurationWorkspace from '../ConfigurationWorkspace.vue'
import type { ConfigValidationResponse } from '../api'
import type { CanonicalConfig, ConfigDiagnostic, ConfigSnapshot, StartupOnlyChange } from '../config'
import { defaultRtmpAutoPush, defaultRtmpCallback, defaultRtmpOutboundPolicy, defaultRtmpRelay } from '../configuration/canonicalDefaults'
import { contractConfigSnapshot, jsonResponse } from './contractFixtures'

export const diskRevision = 'disk-111111111111111111111111111111111111111111111111111111111111'
export const activeRevision = 'active-000000000000000000000000000000000000000000000000000000000'
export const bearerToken = 'test-only-config-token'
export const restartReason: StartupOnlyChange = {
  code: 'listener_ipv6_only',
  path: '/config/listeners/public',
  summary: 'the listener ipv6_only setting changes only on restart',
}

export function canonicalConfig(): CanonicalConfig {
  const expanded = contractConfigSnapshot().config
  return {
    version: 1,
    max_connections: null,
    management: { bind: '127.0.0.1:9080', ui_dir: '/opt/oxiroute/ui/dist' },
    certificates: [
      {
        name: 'direct-public',
        dns_names: ['api.example.test', '*.example.test'],
        source: {
          type: 'files',
          certificate_chain_path: '/etc/oxiroute/public.pem',
          private_key_path: '/etc/oxiroute/public-key.pem',
        },
      },
      {
        name: 'certbot-public',
        dns_names: ['certbot.example.test'],
        source: {
          type: 'certbot',
          live_directory_path: '/etc/letsencrypt/live/certbot.example.test',
          archive_directory_path: '/etc/letsencrypt/archive/certbot.example.test',
        },
      },
    ],
    tls_profiles: [
      {
        name: 'public-sni',
        certificates: ['direct-public', 'certbot-public'],
        default_certificate: 'direct-public',
        min_version: '1.2',
        alpn: ['h2', 'http/1.1'],
         policy: { cipher_list: null, dh_parameters_path: null, client_auth: { mode: 'disabled', ca_certificate_path: null, allowed_dns_names: [] }, session_cache: null, session_timeout_seconds: null, session_tickets: false, prefer_server_ciphers: true, backend: 'openssl', ktls: 'off' },
      },
      {
        name: 'internal-sni',
        certificates: ['direct-public'],
        default_certificate: 'direct-public',
        min_version: '1.3',
        alpn: ['http/1.1'],
         policy: { cipher_list: null, dh_parameters_path: null, client_auth: { mode: 'disabled', ca_certificate_path: null, allowed_dns_names: [] }, session_cache: null, session_timeout_seconds: null, session_tickets: false, prefer_server_ciphers: true, backend: 'openssl', ktls: 'off' },
      },
    ],
    listeners: [
      {
        name: 'https',
        bind: { type: 'socket', address: '0.0.0.0:443' },
        protocol: 'http',
        service: 'web',
        tls_profile: 'public-sni',
        max_connections: 10_000,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
      {
        name: 'rtmp-ingest',
        bind: { type: 'socket', address: '0.0.0.0:1935' },
        protocol: 'rtmp',
        service: 'live',
        tls_profile: null,
        max_connections: 2_000,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
      {
        name: 'database',
        bind: { type: 'unix', path: '/run/oxiroute/postgres.sock', mode: null },
        protocol: 'tcp',
        service: 'postgres',
        tls_profile: null,
        max_connections: null,
        downstream_timeouts: { client_timeout_ms: null, request_timeout_ms: null, keepalive_timeout_ms: null },
      },
    ],
    cache_stores: structuredClone(expanded.cache_stores),
    upstream_pools: [
      {
        name: 'web-origins',
        servers: [
          { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' },
          { name: 'server-2', endpoint: { type: 'dns', host: 'backend.example.test', port: 3001 }, max_connections: null, dns_resolution: 'on_connect' },
        ],
        algorithm: 'round_robin',
        health_check: {
          type: 'http',
          interval_ms: 5_000,
          timeout_ms: 1_000,
          healthy_threshold: 1,
          unhealthy_threshold: 3,
          startup: 'checking',
          fast_interval_ms: null,
          down_interval_ms: null,
          host: 'api.example.test',
          path: '/healthz',
          expected_status: null,
          http_version: null,
        },
        passive_health: null,
        tls: null,
        http_versions: { min: '1.1', max: '1.1' },
        queue_timeout_ms: null,
        connect_timeout_ms: null,
        server_timeout_ms: null,
        connection_reuse: 'safe',
        resilience: {
          fail_open: false,
          min_members: null,
          ejection_jitter_percent: 0,
          queue_while_unavailable_ms: null,
          unavailable_retry_after_seconds: null,
          unavailable_body: null,
        },
      },
      {
        name: 'secure-origins',
        servers: [{ name: 'server-1', endpoint: { type: 'socket', address: '192.0.2.88:443' }, max_connections: null, dns_resolution: 'on_connect' }],
        algorithm: 'round_robin',
        health_check: null,
        passive_health: null,
        tls: {
          server_name: 'origin.example.test',
          ca_certificate_path: '/etc/oxiroute/origin-ca.pem',
        },
        http_versions: { min: '1.1', max: '2' },
        queue_timeout_ms: null,
        connect_timeout_ms: null,
        server_timeout_ms: null,
        connection_reuse: 'safe',
        resilience: {
          fail_open: false,
          min_members: null,
          ejection_jitter_percent: 0,
          queue_while_unavailable_ms: null,
          unavailable_retry_after_seconds: null,
          unavailable_body: null,
        },
      },
    ],
    http_services: [
      {
        name: 'web',
        routes: [
          {
            host: { kind: 'normalized_host', value: 'api.example.test' },
            path: { kind: 'segment_prefix', value: '/v1' },
            methods: ['GET', 'POST'],
            access_policy: {
              type: 'bearer_token_file',
              token_file_path: '/run/oxiroute/api-token',
              header_name: 'authorization',
              realm: 'api',
            },
            policy: {
              max_request_body_bytes: 10_485_760,
              connect_timeout_ms: 30_000,
              read_timeout_ms: 30_000,
              write_timeout_ms: 30_000,
              request_buffering: false,
              response_buffering: false,
            },
            action: {
              type: 'proxy',
              upstream_pool: 'web-origins',
              policy: {
                upstream_host: { type: 'endpoint', unix_fallback: 'localhost' },
                request_headers: [{
                  operation: 'set',
                  name: 'x-forwarded-for',
                  value: { type: 'client_ip' },
                }],
                response_headers: [{ operation: 'remove', name: 'server' }],
                response_cookie_path_rewrites: [{ from: '/', to: '/v1' }],
                response_cookie_attributes: [],
                retry: {
                  max_retries: 1,
                  target: 'next_server',
                  delay_ms: 0,
                  final_redispatch: false,
                  jitter_percent: 0,
                  idempotency_header: null,
                  budget: null,
                  triggers: ['connect_failure', 'connect_timeout', 'refused_stream'],
                  method_safety: 'get_head',
                  body_safety: 'empty',
                },
                timeouts: { connect_ms: null, header_ms: null, idle_ms: null, total_ms: null },
                rescue: null,
                cache: structuredClone(expanded.http_services[0]!.routes[0]!.action.type === 'proxy'
                  ? expanded.http_services[0]!.routes[0]!.action.policy.cache
                  : null),
              },
            },
          },
        ],
        automatic_response_headers: true,
        upstream_io_timeout_ms: 30_000,
        max_request_body_bytes: 10_485_760,
        gzip: null,
        access_log: null,
      },
    ],
    forward_proxy_services: structuredClone(expanded.forward_proxy_services),
    rtmp_services: [
      {
        name: 'live',
        outbound_chunk_size: 4_096,
        access_log: null,
        outbound_policy: defaultRtmpOutboundPolicy(),
        callbacks: defaultRtmpCallback(),
        auto_push: defaultRtmpAutoPush(),
        applications: [
          {
            name: 'broadcast',
            live: true,
            idle_streams: true,
            publish: { rules: [], token: null },
            play: { rules: [], token: null },
            limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
            push_targets: [],
            pull_targets: [],
            relay: defaultRtmpRelay(),
            callbacks: defaultRtmpCallback(),
            fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
            vod: null,
            recorders: [
              {
                name: 'archive',
                start: 'continuous',
                root_directory: '/var/lib/oxiroute/recordings',
                record_mask: { audio: true, video: true, keyframes: false },
                suffix_template: '-%Y-%m-%dT%H-%M-%S.flv',
                append_unix_seconds: false,
                append: false,
                lock: false,
                max_size: null,
                max_frames: null,
                notify: false,
                timezone: 'utc',
                time_basis: 'segment_start',
                segment_naming: 'safe_unique',
                rotation_interval_ms: null,
                max_queue_messages: 256,
                max_queue_bytes: 8_388_608,
                shutdown_timeout_ms: 5_000,
                max_storage_bytes: 10_737_418_240,
                max_storage_files: 10_000,
                max_active_recorders: 8,
                retention: 'reject',
                retention_max_age_ms: null,
              },
            ],
          },
        ],
      },
    ],
    l4_services: [
      {
        name: 'postgres',
        upstream_pool: 'web-origins',
        connect_timeout_ms: 10_000,
        idle_timeout_ms: 300_000,
        lifetime_timeout_ms: 3_600_000,
        udp: null,
      },
    ],
  }
}

export function configSnapshot(): ConfigSnapshot {
  return {
    schemaVersion: 1,
    diskRevision,
    candidateRevision: 'candidate-1111111111111111111111111111111111111111111111111111111',
    activeRevision,
    config: canonicalConfig(),
    configFormat: 'kdl',
    compositional: false,
    dependencyCount: 0,
    configPreview: 'version 1\n',
    diagnostics: [],
  }
}

export function validationResponse(config: CanonicalConfig, diagnostics: ConfigDiagnostic[] = []): ConfigValidationResponse {
  return {
    candidateRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
    normalizedConfig: structuredClone(config),
    configFormat: 'kdl',
    compositional: false,
    dependencyCount: 0,
    configPreview: `version ${config.version}\n`,
    diagnostics,
    restartRequired: false,
    restartReasons: [],
    topology: {
      schemaVersion: 1,
      state: { config: 'candidate', runtime: 'not_active', sampledAtUnixMs: 1_750_000_000_000 },
      nodes: [
        {
          id: 'listener:5:https',
          kind: 'listener',
          name: config.listeners[0]?.name ?? 'https',
          configPath: '/listeners/0',
          attributes: { protocol: 'http' },
        },
      ],
      edges: [],
      overlays: [],
    },
  }
}

export function findButton(wrapper: VaporWrapper, text: string) {
  const button = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!button) throw new Error(`Button not found: ${text}`)
  return button
}

export function findButtonIn(
  wrapper: { findAll(selector: string): Array<{ text(): string; trigger(event: string): Promise<void> }> },
  text: string,
) {
  const button = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!button) throw new Error(`Button not found: ${text}`)
  return button
}

export async function mountUnlocked(attachToBody = false): Promise<VaporWrapper> {
  const wrapper = mount(ConfigurationWorkspace, {
    props: { token: bearerToken },
    ...(attachToBody ? { attachTo: document.body } : {}),
  })
  await flushPromises()
  return wrapper
}

export function installConfigFetch(
  putResponse?: (body: CanonicalConfig) => Response,
  validationStatus = 200,
  snapshot = configSnapshot(),
  validationRestartRequired = false,
  validationDiagnostics: ConfigDiagnostic[] = [],
) {
  const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/config' && !init?.method) return jsonResponse(snapshot)
    if (url === '/api/v1/config/validate') {
      const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
      return validationStatus === 422
        ? jsonResponse({ diagnostics: [invalidDiagnostic] }, 422)
        : jsonResponse({
            ...validationResponse(body.config),
            restartRequired: validationRestartRequired,
            restartReasons: validationRestartRequired ? [restartReason] : [],
            diagnostics: validationDiagnostics,
          })
    }
    if (url === '/api/v1/config' && init?.method === 'PUT') {
      const body = JSON.parse(String(init.body)) as { config: CanonicalConfig }
      if (putResponse) return putResponse(body.config)
      return jsonResponse({
        diskRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        candidateRevision: 'candidate-2222222222222222222222222222222222222222222222222222222',
        activeRevision,
        outcome: 'saved_pending_activation',
        activationState: 'pending',
        restartRequired: false,
        restartReasons: [],
        diagnostics: [{
          code: 'I_ACTIVATION_PENDING',
          severity: 'warning',
          stage: 'activation',
          message: 'configuration was saved and queued for generation activation',
        }],
      })
    }
    throw new Error(`Unexpected request: ${url}`)
  })
  vi.stubGlobal('fetch', fetch)
  return fetch
}

export const invalidDiagnostic: ConfigDiagnostic = {
  code: 'E_UNRESOLVED_REFERENCE',
  severity: 'error',
  stage: 'validation',
  path: '/listeners/0/service',
  message: 'Listener references an unknown HTTP service.',
}

afterEach(() => {
  Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1024 })
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  document.body.innerHTML = ''
})
