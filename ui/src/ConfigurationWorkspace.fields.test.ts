import { flushPromises } from './test/mount'
import { describe, expect, it } from 'vitest'

import { CANONICAL_FIELD_REGISTRY, isCanonicalConfig } from './config'
import type { CanonicalConfig } from './config'
import { defaultRtmpAutoPush, defaultRtmpCallback, defaultRtmpOutboundPolicy, defaultRtmpRelay } from './configuration/canonicalDefaults'
import {
  canonicalConfig,
  configSnapshot,
  findButton,
  findButtonIn,
  installConfigFetch,
  mountUnlocked,
} from './test/configurationWorkspace'

describe('ConfigurationWorkspace', () => {
  it('exposes exactly the canonical registry through stable field controls', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    const observed = new Set<string>()
    const captureFields = () => {
      for (const field of wrapper.findAll<HTMLElement>('[data-field]')) {
        const path = field.attributes('data-field')
        expect(path).toBeDefined()
        observed.add(path!)
        expect(field.find('input, select, textarea, button').exists()).toBe(true)
      }
    }

    captureFields()
    for (const key of [
      'management',
      'certificates:0',
      'certificates:1',
      'tls_profiles:0',
      'listeners:0',
      'cache_stores:0',
      'cache_stores:1',
      'upstream_pools:0',
      'upstream_pools:1',
      'http_services:0',
      'forward_proxy_services:0',
      'rtmp_services:0',
      'l4_services:0',
    ]) {
      await wrapper.get('#mobile-object-navigation').setValue(key)
      captureFields()
      if (key === 'listeners:0') {
        await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('unix')
        captureFields()
        await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('socket')
      }
      if (key === 'upstream_pools:1') {
        await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.type"] select').setValue('unix')
        captureFields()
      }
      if (key === 'http_services:0') {
        const upstreamHost = wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.type"] select')
        await upstreamHost.setValue('literal')
        captureFields()

        const requestValue = wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.type"] select')
        await requestValue.setValue('literal')
        captureFields()

        await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].operation"] select').setValue('set')
        captureFields()

        const actionType = wrapper.get('[data-field="http_services[].routes[].action.type"] select')
        await actionType.setValue('fixed_response')
        await findButton(wrapper, 'Add header').trigger('click')
        captureFields()
        await actionType.setValue('redirect')
        captureFields()
        await actionType.setValue('static_files')
        captureFields()
      }
    }

    const registered = new Set<string>(CANONICAL_FIELD_REGISTRY.map(({ path }) => path))
    expect([...observed].every((path) => registered.has(path))).toBe(true)
    expect(observed).toContain('upstream_pools[].servers[].name')
    expect(observed).toContain('upstream_pools[].servers[].endpoint.type')
  })

  it('adds, removes, edits, and validates every canonical field control', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()
    const selectObject = (key: string) => wrapper.get('#mobile-object-navigation').setValue(key)

    await wrapper.get('[data-field="version"] input').setValue(2)

    await selectObject('management')
    const managementToggle = wrapper.get('[data-field="management"] input')
    await managementToggle.setValue(false)
    await managementToggle.setValue(true)
    await wrapper.get('[data-field="management.bind"] input').setValue('127.0.0.1:9081')
    await wrapper.get('[data-field="management.ui_dir"] input').setValue('/srv/oxiroute/ui')

    await selectObject('certificates:0')
    await wrapper.get('[data-field="certificates[].name"] input').setValue('direct-edge')
    const directDns = wrapper.get('[data-field="certificates[].dns_names"]')
    await directDns.findAll('input')[0]!.setValue('edge.example.test')
    await findButtonIn(directDns, 'Add DNS name').trigger('click')
    await directDns.findAll('input').at(-1)!.setValue('temporary.example.test')
    await directDns.findAll('.remove-row').at(-1)!.trigger('click')
    const sourceType = wrapper.get('[data-field="certificates[].source.type"] select')
    await sourceType.setValue('certbot')
    await sourceType.setValue('acme_managed')
    await wrapper.get('[data-field="certificates[].source.directory_url"] input').setValue('https://acme.test/directory')
    await wrapper.get('[data-field="certificates[].source.state_root"] input').setValue('/var/lib/oxiroute/acme')
    await wrapper.get('[data-field="certificates[].source.terms_agreed"] input').setValue(true)
    await wrapper.get('[data-field="certificates[].source.challenge"] select').setValue('dns01')
    await wrapper.get('[data-field="certificates[].source.dns01.provider"] input').setValue('fake')
    await wrapper.get('[data-field="certificates[].source.dns01.credential_file"] input').setValue('/etc/oxiroute/dns-credentials')
    await wrapper.get('[data-field="certificates[].source.dns01.timeout_seconds"] input').setValue(30)
    await wrapper.get('[data-field="certificates[].source.allowed_dns_suffixes"] .add-row').trigger('click')
    await wrapper.get('[data-field="certificates[].source.allowed_dns_suffixes"] input').setValue('example.test')
    await sourceType.setValue('self_signed_development')
    expect(wrapper.get('[data-field="certificates[].source.validity_days"] input').element).toHaveProperty('value', '7')
    await wrapper.get('[data-field="certificates[].source.validity_days"] input').setValue(14)
    await wrapper.get('[data-field="certificates[].source.key_type"] select').setValue('rsa_2048')
    await sourceType.setValue('files')
    await wrapper.get('[data-field="certificates[].source.certificate_chain_path"] input').setValue('/etc/edge-chain.pem')
    await wrapper.get('[data-field="certificates[].source.private_key_path"] input').setValue('/etc/edge-key.pem')

    await selectObject('certificates:1')
    await wrapper.get('[data-field="certificates[].name"] input').setValue('certbot-edge')
    await wrapper.get('[data-field="certificates[].dns_names"] input').setValue('certbot-edge.example.test')
    await wrapper.get('[data-field="certificates[].source.live_directory_path"] input').setValue('/etc/letsencrypt/live/edge')
    await wrapper.get('[data-field="certificates[].source.archive_directory_path"] input').setValue('/etc/letsencrypt/archive/edge')

    await selectObject('tls_profiles:0')
    expect(wrapper.get('[data-field="tls_profiles[].default_certificate"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="tls_profiles[].name"] input').setValue('edge-sni')
    const profileCertificates = wrapper.get('[data-field="tls_profiles[].certificates"]')
    await profileCertificates.findAll('input')[0]!.setValue('direct-edge')
    await profileCertificates.findAll('input')[1]!.setValue('certbot-edge')
    await findButtonIn(profileCertificates, 'Add certificate reference').trigger('click')
    await profileCertificates.findAll('.remove-row').at(-1)!.trigger('click')
    await wrapper.get('[data-field="tls_profiles[].default_certificate"] select').setValue('direct-edge')
    await wrapper.get('[data-field="tls_profiles[].min_version"] select').setValue('1.3')
    await wrapper.get('[data-field="tls_profiles[].alpn"] select').setValue('h2')
    await wrapper.get('[data-field="tls_profiles[].policy.cipher_list"] input').setValue('ECDHE-RSA-AES128-GCM-SHA256')
    await wrapper.get('[data-field="tls_profiles[].policy.dh_parameters_path"] input').setValue('/etc/letsencrypt/ssl-dhparams.pem')
    await wrapper.get('[data-field="tls_profiles[].policy.session_cache.name"] input').setValue('le_nginx_SSL')
    await wrapper.get('[data-field="tls_profiles[].policy.session_cache.size_bytes"] input').setValue(10 * 1024 * 1024)
    await wrapper.get('[data-field="tls_profiles[].policy.session_timeout_seconds"] input').setValue(86_400)
    await wrapper.get('[data-field="tls_profiles[].policy.session_tickets"] input').setValue(true)
    await wrapper.get('[data-field="tls_profiles[].policy.session_tickets"] input').setValue(false)
    await wrapper.get('[data-field="tls_profiles[].policy.prefer_server_ciphers"] input').setValue(false)
    await wrapper.get('[data-field="tls_profiles[].policy.backend"] select').setValue('rustls')
    await wrapper.get('[data-field="tls_profiles[].policy.backend"] select').setValue('openssl')
    await wrapper.get('[data-field="tls_profiles[].policy.key_exchange_groups"] input').setValue('x25519mlkem768, x25519')
    await wrapper.get('[data-field="tls_profiles[].policy.ktls"] select').setValue('auto')
    await wrapper.get('[data-field="tls_profiles[].policy.client_auth.mode"] select').setValue('optional')
    await wrapper.get('[data-field="tls_profiles[].policy.client_auth.ca_certificate_path"] input').setValue('/etc/oxiroute/client-ca.pem')
    const allowedClientSans = wrapper.get('[data-field="tls_profiles[].policy.client_auth.allowed_dns_names"]')
    await findButtonIn(allowedClientSans, 'Add exact DNS or IP SAN').trigger('click')
    await allowedClientSans.find('input').setValue('client.example.test')

    await selectObject('listeners:0')
    await wrapper.get('[data-field="listeners[].name"] input').setValue('edge-https')
    const bindType = wrapper.get('[data-field="listeners[].bind.type"] select')
    await bindType.setValue('unix')
    await wrapper.get('[data-field="listeners[].bind.path"] input').setValue('/run/oxiroute/edge.sock')
    await bindType.setValue('socket')
    await wrapper.get('[data-field="listeners[].bind.address"] input').setValue('127.0.0.1:8443')
    const listenerProtocol = wrapper.get('[data-field="listeners[].protocol"] select')
    await listenerProtocol.setValue('tcp')
    await listenerProtocol.setValue('http')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('web')
    await wrapper.get('[data-field="listeners[].tls_profile"] select').setValue('edge-sni')
    await wrapper.get('[data-field="listeners[].max_connections"] input').setValue(12_000)

    await selectObject('upstream_pools:0')
    await wrapper.get('[data-field="upstream_pools[].name"] input').setValue('origins-a')
    const servers = wrapper.get('[data-field="upstream_pools[].servers"]')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.address"] input').setValue('127.0.0.1:3100')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.host"] input').setValue('edge-backend.example.test')
    await servers.find('[data-field="upstream_pools[].servers[].endpoint.port"] input').setValue(3101)
    await findButtonIn(servers, 'Add server').trigger('click')
    await wrapper.get('[aria-label="Remove upstream server 3"]').trigger('click')
    await wrapper.get('[data-field="upstream_pools[].algorithm"] select').setValue('least_connections')
    const health = wrapper.get('[data-field="upstream_pools[].health_check"]')
    const healthToggle = health.find('.enable-row input')
    await healthToggle.setValue(false)
    await healthToggle.setValue(true)
    const healthType = wrapper.get('[data-field="upstream_pools[].health_check.type"] select')
    await healthType.setValue('http')
    await wrapper.get('[data-field="upstream_pools[].health_check.interval_ms"] input').setValue(6_000)
    await wrapper.get('[data-field="upstream_pools[].health_check.timeout_ms"] input').setValue(1_500)
    await wrapper.get('[data-field="upstream_pools[].health_check.healthy_threshold"] input').setValue(2)
    await wrapper.get('[data-field="upstream_pools[].health_check.unhealthy_threshold"] input').setValue(4)
    await wrapper.get('[data-field="upstream_pools[].health_check.host"] input').setValue('edge.example.test')
    await wrapper.get('[data-field="upstream_pools[].health_check.path"] input').setValue('/ready')
    const passive = wrapper.get('[data-field="upstream_pools[].passive_health"]')
    const passiveToggle = passive.find('.enable-row input')
    await passiveToggle.setValue(true)
    await wrapper.get('[data-field="upstream_pools[].passive_health.observe"] select').setValue('layer4')
    await wrapper.get('[data-field="upstream_pools[].passive_health.on_error"] select').setValue('mark_down')
    await wrapper.get('[data-field="upstream_pools[].passive_health.error_limit"] input').setValue(4)
    await wrapper.get('[data-field="upstream_pools[].passive_health.mark_down"] input').setValue(true)
    await wrapper.get('[data-field="upstream_pools[].passive_health.mark_up"] input').setValue(true)
    await wrapper.get('[data-field="upstream_pools[].passive_health.initial_backoff_ms"] input').setValue(30_000)
    await wrapper.get('[data-field="upstream_pools[].passive_health.max_backoff_ms"] input').setValue(300_000)
    await wrapper.get('[data-field="upstream_pools[].passive_health.recovery_threshold"] input').setValue(2)
    await wrapper.get('[data-field="upstream_pools[].http_versions.min"] select').setValue('1.1')
    expect(wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]').attributes()).toHaveProperty('disabled')

    await selectObject('upstream_pools:1')
    const upstreamTls = wrapper.get('[data-field="upstream_pools[].tls"]')
    const upstreamTlsToggle = upstreamTls.find('.enable-row input')
    await upstreamTlsToggle.setValue(false)
    await upstreamTlsToggle.setValue(true)
    await wrapper.get('[data-field="upstream_pools[].tls.server_name"] input').setValue('secure-edge.example.test')
    await wrapper.get('[data-field="upstream_pools[].tls.ca_certificate_path"] input').setValue('/etc/secure-edge-ca.pem')
    await wrapper.get('[data-field="upstream_pools[].http_versions.max"] select').setValue('2')

    await selectObject('http_services:0')
    await wrapper.get('[data-field="http_services[].name"] input').setValue('web-edge')
    await wrapper.get('[data-field="http_services[].automatic_response_headers"] input').setValue(false)
    await wrapper.get('[data-field="http_services[].upstream_io_timeout_ms"] input').setValue(31_000)
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] select').setValue('unbounded')
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] select').setValue('bounded')
    await wrapper.get('[data-field="http_services[].max_request_body_bytes"] input').setValue(20_000_000)
    await wrapper.get('[data-field="http_services[].routes[].host.kind"] select').setValue('exact_authority')
    await wrapper.get('[data-field="http_services[].routes[].host.value"] input').setValue('edge.example.test:8443')
    await wrapper.get('[data-field="http_services[].routes[].path.kind"] select').setValue('exact')
    await wrapper.get('[data-field="http_services[].routes[].path.value"] input').setValue('/edge')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.token_file_path"] input').setValue('/run/oxiroute/edge-token')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.header_name"] input').setValue('x-api-token')
    await wrapper.get('[data-field="http_services[].routes[].access_policy.realm"] input').setValue('edge')
    expect(wrapper.get('[data-field="http_services[].routes[].action.upstream_pool"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="http_services[].routes[].action.upstream_pool"] select').setValue('origins-a')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.type"] select').setValue('literal')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.upstream_host.value"] input').setValue('origin.example.test')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].name"] input').setValue('x-client')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.type"] select').setValue('literal')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.request_headers[].value.value"] input').setValue('edge-client')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].operation"] select').setValue('set')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].name"] input').setValue('x-edge')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_headers[].value"] input').setValue('active')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].from"] input').setValue('/v1')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.response_cookie_path_rewrites[].to"] input').setValue('/edge')
    await wrapper.get('[data-field="http_services[].routes[].action.policy.retry.max_retries"] input').setValue(2)
    const methods = wrapper.get('[data-field="http_services[].routes[].methods"]')
    await methods.findAll('input')[0]!.setValue('PUT')
    await findButtonIn(methods, 'Add method').trigger('click')
    await methods.findAll('.remove-row').at(-1)!.trigger('click')
    await findButton(wrapper, 'Add route').trigger('click')
    await wrapper.get('[aria-label="Remove route 2"]').trigger('click')

    await selectObject('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].name"] input').setValue('media-edge')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('publish')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].live"] input').attributes())
      .toHaveProperty('disabled')
    await wrapper.get('[data-field="rtmp_services[].applications[].idle_streams"] input').setValue(false)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].name"] input').setValue('manual-archive')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].start"] select').setValue('manual')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].root_directory"] input').setValue('/srv/recordings')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].suffix_template"] input').setValue('-%Y%m%d.flv')
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].append_unix_seconds"] input').setValue(true)
    const rotation = wrapper.get('[data-field="rtmp_services[].applications[].recorders[].rotation_interval_ms"]')
    await rotation.get('select').setValue('bounded')
    await rotation.get('input').setValue(120_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_queue_messages"] input').setValue(512)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_queue_bytes"] input').setValue(16_777_216)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].shutdown_timeout_ms"] input').setValue(6_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_storage_bytes"] input').setValue(21_474_836_480)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_storage_files"] input').setValue(20_000)
    await wrapper.get('[data-field="rtmp_services[].applications[].recorders[].max_active_recorders"] input').setValue(16)
    await findButton(wrapper, 'Add recorder').trigger('click')
    await wrapper.get('[aria-label="Remove recorder 2"]').trigger('click')
    await findButton(wrapper, 'Add application').trigger('click')
    await wrapper.get('[aria-label="Remove RTMP application 2"]').trigger('click')

    await selectObject('l4_services:0')
    await wrapper.get('[data-field="l4_services[].name"] input').setValue('database-edge')
    expect(wrapper.get('[data-field="l4_services[].upstream_pool"] select').attributes()).toHaveProperty('required')
    await wrapper.get('[data-field="l4_services[].upstream_pool"] select').setValue('origins-a')
    await wrapper.get('[data-field="l4_services[].connect_timeout_ms"] input').setValue(11_000)
    await wrapper.get('[data-field="l4_services[].idle_timeout_ms"] input').setValue(301_000)
    await wrapper.get('[data-field="l4_services[].lifetime_timeout_ms"] input').setValue(3_601_000)

    for (const [addLabel, removeLabel] of [
      ['Add certificate', 'Remove certificate'],
      ['Add SNI profile', 'Remove profile'],
      ['Add listener', 'Remove listener'],
      ['Add upstream pool', 'Remove pool'],
      ['Add HTTP service', 'Remove service'],
      ['Add RTMP service', 'Remove service'],
      ['Add L4 service', 'Remove service'],
    ] as const) {
      await wrapper.get(`[aria-label="${addLabel}"]`).trigger('click')
      await findButton(wrapper, removeLabel).trigger('click')
    }

    await selectObject('listeners:0')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('web-edge')
    await selectObject('listeners:1')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('media-edge')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted).toEqual(expect.objectContaining({
      version: 2,
      management: { bind: '127.0.0.1:9081', ui_dir: '/srv/oxiroute/ui' },
    }))
    expect(submitted.certificates[0]).toEqual(expect.objectContaining({
      name: 'direct-edge',
      source: { type: 'files', certificate_chain_path: '/etc/edge-chain.pem', private_key_path: '/etc/edge-key.pem' },
    }))
    expect(submitted.tls_profiles[0]).toEqual(expect.objectContaining({
      name: 'edge-sni',
      certificates: ['direct-edge', 'certbot-edge'],
      default_certificate: 'direct-edge',
      min_version: '1.3',
      alpn: ['h2'],
      policy: {
         cipher_list: 'ECDHE-RSA-AES128-GCM-SHA256',
         dh_parameters_path: '/etc/letsencrypt/ssl-dhparams.pem',
         client_auth: {
           mode: 'optional',
           ca_certificate_path: '/etc/oxiroute/client-ca.pem',
           allowed_dns_names: ['client.example.test'],
         },
         session_cache: { name: 'le_nginx_SSL', size_bytes: 10 * 1024 * 1024 },
        session_timeout_seconds: 86_400,
        session_tickets: false,
        prefer_server_ciphers: false,
        backend: 'openssl',
        key_exchange_groups: ['x25519mlkem768', 'x25519'],
        ktls: 'auto',
      },
    }))
    expect(submitted.upstream_pools[0]).toEqual(expect.objectContaining({
      name: 'origins-a',
      servers: [
        { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3100' }, max_connections: null, dns_resolution: 'on_connect' },
        { name: 'server-2', endpoint: { type: 'dns', host: 'edge-backend.example.test', port: 3101 }, max_connections: null, dns_resolution: 'on_connect' },
      ],
      algorithm: 'least_connections',
      health_check: expect.objectContaining({ host: 'edge.example.test', path: '/ready' }),
      http_versions: { min: '1.1', max: '1.1' },
    }))
    expect(submitted.http_services[0]).toEqual(expect.objectContaining({
      name: 'web-edge',
      automatic_response_headers: false,
      upstream_io_timeout_ms: 31_000,
      max_request_body_bytes: 20_000_000,
    }))
    expect(submitted.http_services[0]?.routes[0]).toEqual({
      host: { kind: 'exact_authority', value: 'edge.example.test:8443' },
      path: { kind: 'exact', value: '/edge' },
      methods: ['PUT', 'POST'],
      access_policy: {
        type: 'bearer_token_file',
        token_file_path: '/run/oxiroute/edge-token',
        header_name: 'x-api-token',
        realm: 'edge',
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
        upstream_pool: 'origins-a',
        policy: {
          upstream_host: { type: 'literal', value: 'origin.example.test' },
          request_headers: [{
            operation: 'set',
            name: 'x-client',
            value: { type: 'literal', value: 'edge-client' },
          }],
          response_headers: [{ operation: 'set', name: 'x-edge', value: 'active', always: true }],
          response_cookie_path_rewrites: [{ from: '/v1', to: '/edge' }],
          response_cookie_attributes: [],
          retry: {
            max_retries: 2,
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
          cache: expect.objectContaining({ store: 'responses' }),
        },
      },
    })
    expect(submitted.rtmp_services[0]).toEqual({
      name: 'media-edge',
      outbound_chunk_size: 4_096,
      access_log: null,
      outbound_policy: defaultRtmpOutboundPolicy(),
      callbacks: defaultRtmpCallback(),
      auto_push: defaultRtmpAutoPush(),
      applications: [{
        name: 'publish',
        live: true,
        idle_streams: false,
        publish: { rules: [], token: null },
        play: { rules: [], token: null },
        limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
        push_targets: [],
        pull_targets: [],
        relay: defaultRtmpRelay(),
        callbacks: defaultRtmpCallback(),
        fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
        vod: null,
        recorders: [{
          name: 'manual-archive',
          start: 'manual',
          root_directory: '/srv/recordings',
          record_mask: { audio: true, video: true, keyframes: false },
          suffix_template: '-%Y%m%d.flv',
          append_unix_seconds: true,
          append: false,
          lock: false,
          max_size: null,
          max_frames: null,
          notify: false,
          timezone: 'utc',
          time_basis: 'segment_start',
          segment_naming: 'safe_unique',
          rotation_interval_ms: 120_000,
          max_queue_messages: 512,
          max_queue_bytes: 16_777_216,
          shutdown_timeout_ms: 6_000,
          max_storage_bytes: 21_474_836_480,
          max_storage_files: 20_000,
          max_active_recorders: 16,
        }],
      }],
    })
    expect(submitted.l4_services[0]).toEqual({
      name: 'database-edge',
      upstream_pool: 'origins-a',
      connect_timeout_ms: 11_000,
      idle_timeout_ms: 301_000,
      lifetime_timeout_ms: 3_601_000,
      udp: null,
    })
  })

  it('serializes TLS-ALPN-01 without stale DNS provider state and passes the canonical guard', async () => {
    const snapshot = configSnapshot()
    snapshot.config.certificates[0]!.source = {
      type: 'acme_managed',
      directory_url: 'https://acme.example.test/directory',
      state_root: '/var/lib/oxiroute/acme',
      contacts: ['mailto:ops@example.test'],
      terms_agreed: true,
      challenge: 'http01',
      key_type: 'ecdsa_p256',
      allowed_dns_suffixes: ['example.test'],
      retained_revisions: 3,
      retention_days: 30,
      dns01: null,
    }
    const fetch = installConfigFetch(undefined, 200, snapshot)
    const wrapper = await mountUnlocked()
    await wrapper.get('#mobile-object-navigation').setValue('certificates:0')

    const challenge = wrapper.get('[data-field="certificates[].source.challenge"] select')
    await challenge.setValue('dns01')
    await wrapper.get('[data-field="certificates[].source.dns01.provider"] input').setValue('route53')
    await wrapper.get('[data-field="certificates[].source.dns01.credential_file"] input').setValue('/run/dns-token')
    await challenge.setValue('tls_alpn01')
    expect(wrapper.find('[data-field="certificates[].source.dns01.provider"]').exists()).toBe(false)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(isCanonicalConfig(submitted)).toBe(true)
    expect(submitted.certificates[0]?.source).toEqual({
      type: 'acme_managed',
      directory_url: 'https://acme.example.test/directory',
      state_root: '/var/lib/oxiroute/acme',
      contacts: ['mailto:ops@example.test'],
      terms_agreed: true,
      challenge: 'tls_alpn01',
      key_type: 'ecdsa_p256',
      allowed_dns_suffixes: ['example.test'],
      retained_revisions: 3,
      retention_days: 30,
      dns01: null,
    })
    expect(JSON.stringify(submitted)).not.toContain('route53')
    expect(JSON.stringify(submitted)).not.toContain('/run/dns-token')
  })

  it('edits RTMP listeners and every RTMP service field with protocol-aware references', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:1')
    const service = wrapper.get('[data-field="listeners[].service"] select')
    expect(service.findAll('option').map((option) => option.text())).toEqual(['Select a service', 'live'])
    expect(service.text()).not.toContain('web')
    expect(service.text()).not.toContain('postgres')

    await wrapper.get('[data-field="listeners[].bind.address"] input').setValue('127.0.0.1:1936')
    await wrapper.get('[data-field="listeners[].protocol"] select').setValue('http')
    expect((service.element as HTMLSelectElement).value).toBe('web')
    expect(service.text()).toContain('web')
    expect(service.text()).not.toContain('live')
    await wrapper.get('[data-field="listeners[].protocol"] select').setValue('rtmp')
    expect((service.element as HTMLSelectElement).value).toBe('live')

    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].name"] input').setValue('live-edge')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('camera')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].live"] input').attributes())
      .toHaveProperty('disabled')
    await wrapper.get('[data-field="rtmp_services[].applications[].idle_streams"] input').setValue(false)

    await findButton(wrapper, 'Add application').trigger('click')
    expect(wrapper.findAll('[data-field="rtmp_services[].applications[].name"]')).toHaveLength(2)
    await wrapper.get('[aria-label="Remove RTMP application 2"]').trigger('click')
    expect(wrapper.findAll('[data-field="rtmp_services[].applications[].name"]')).toHaveLength(1)

    await wrapper.get('[aria-label="Add RTMP service"]').trigger('click')
    expect(wrapper.get('[data-field="rtmp_services[].applications[].name"] input').element).toBeInstanceOf(HTMLInputElement)
    await findButton(wrapper, 'Remove service').trigger('click')
    await wrapper.get('[aria-label="Add listener"]').trigger('click')
    await findButton(wrapper, 'Remove listener').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('listeners:1')
    await wrapper.get('[data-field="listeners[].service"] select').setValue('live-edge')
    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.listeners[1]).toEqual(expect.objectContaining({
      bind: { type: 'socket', address: '127.0.0.1:1936' },
      protocol: 'rtmp',
      service: 'live-edge',
      tls_profile: null,
    }))
    expect(submitted.rtmp_services).toEqual([
      {
        name: 'live-edge',
        outbound_chunk_size: 4_096,
        access_log: null,
        outbound_policy: defaultRtmpOutboundPolicy(),
        callbacks: defaultRtmpCallback(),
        auto_push: defaultRtmpAutoPush(),
        applications: [{
          name: 'camera',
          live: true,
          idle_streams: false,
          publish: { rules: [], token: null },
          play: { rules: [], token: null },
          limits: { max_connections: 1_024, max_publishers: 256, max_viewers: 1_024 },
          push_targets: [],
          pull_targets: [],
          relay: defaultRtmpRelay(),
          callbacks: defaultRtmpCallback(),
          fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 },
          vod: null,
          recorders: canonicalConfig().rtmp_services[0]!.applications[0]!.recorders,
        }],
      },
    ])
  })

  it('serializes explicit unbounded limits as null and never substitutes zero', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')
    const listenerLimit = wrapper.get('[data-field="listeners[].max_connections"]')
    await listenerLimit.get('input').setValue(0)
    expect((listenerLimit.get('input').element as HTMLInputElement).value).toBe('10000')
    await listenerLimit.get('select').setValue('unbounded')

    await wrapper.get('#mobile-object-navigation').setValue('http_services:0')
    const bodyLimit = wrapper.get('[data-field="http_services[].max_request_body_bytes"]')
    await bodyLimit.get('input').setValue(0)
    expect((bodyLimit.get('input').element as HTMLInputElement).value).toBe('10485760')
    await bodyLimit.get('select').setValue('unbounded')

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.listeners[0]).toHaveProperty('max_connections', null)
    expect(submitted.http_services[0]).toHaveProperty('max_request_body_bytes', null)
  })
})
