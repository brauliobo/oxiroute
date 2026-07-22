import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import type { MonitoringSnapshot, RtmpCatalog } from './api'

const exactHealthCounter = '18446744073709551615'

const catalog: RtmpCatalog = {
  revision: '4',
  as_of_unix_ms: 1_750_000_000_000,
  capabilities: {
    live_ingest: true,
    manual_recording: true,
  },
  streams: [
    {
      id: '2a130dea-5db7-43e0-afb8-f07c4bcb1814',
      revision: '3',
      server_id: 'edge',
      application: 'live',
      name: 'camera',
      created_at_unix_ms: 1_750_000_000_000,
      publisher: {
        session_id: '750a865d-1b72-4a5f-a54b-a1d8510d055c',
        attached_at_unix_ms: 1_750_000_000_000,
      },
      subscriber_count: 12,
      media: {
        audio: {
          codec_id: 10,
          codec_name: 'aac',
          payload_bytes: '1024',
          last_rtmp_timestamp_ms: 120,
          last_observed_at_unix_ms: 1_750_000_000_200,
        },
        video: {
          codec_id: 7,
          codec_name: 'avc',
          payload_bytes: '4096',
          last_rtmp_timestamp_ms: 123,
          last_observed_at_unix_ms: 1_750_000_000_200,
        },
        fanout_payload_bytes: '8192',
      },
      recorders: [
        {
          id: 'c76ad8c2-e575-4989-8fae-1a95566ff598',
          name: 'archive',
          manual: true,
          phase: { state: 'idle' },
          changed_at_unix_ms: 1_750_000_000_000,
          bytes_written: '0',
        },
      ],
    },
  ],
}

function monitoringSample(): MonitoringSnapshot {
  return {
    sampledAtUnixMs: Date.now(),
    uptimeMs: 90_610_000,
    process: {
      cpuPercent: 12.5,
      residentMemoryBytes: 268_435_456,
      virtualMemoryBytes: 1_073_741_824,
      threadCount: 8,
      openFileDescriptors: 42,
    },
    host: {
      loadAverage1m: 0.42,
      loadAverage5m: 0.31,
      loadAverage15m: 0.25,
      totalMemoryBytes: 17_179_869_184,
      availableMemoryBytes: 4_294_967_296,
    },
    traffic: {
      acceptedConnections: 12_345,
      activeConnections: 42,
      bytesReceived: 1_572_864,
      bytesSent: 2_147_483_648,
    },
    listeners: [
      {
        name: 'HTTP ingress',
        protocol: 'http',
        bind: '127.0.0.1:8080',
        maxConnections: 1_000,
        acceptedConnections: 8_000,
        activeConnections: 14,
        bytesReceived: 1_048_576,
        bytesSent: 524_288,
      },
      {
        name: 'Live edge',
        protocol: 'rtmp',
        bind: '0.0.0.0:1935',
        maxConnections: 100,
        acceptedConnections: 4_345,
        activeConnections: 28,
        bytesReceived: 524_288,
        bytesSent: 2_146_959_360,
      },
    ],
    upstreamPools: [
      {
        name: 'web-backends',
        algorithm: 'round_robin',
        availableEndpoints: 1,
        totalEndpoints: 2,
        unavailableSelections: exactHealthCounter,
        endpoints: [
          {
            address: '127.0.0.1:3000',
            state: 'healthy',
            lastCheckedAtUnixMs: Date.now(),
            lastTransitionAtUnixMs: Date.now(),
            successfulChecks: exactHealthCounter,
            failedChecks: '1',
            consecutiveSuccesses: '4',
            consecutiveFailures: '0',
            lastFailure: null,
          },
          {
            address: '127.0.0.1:3001',
            state: 'unhealthy',
            lastCheckedAtUnixMs: Date.now(),
            lastTransitionAtUnixMs: Date.now(),
            successfulChecks: '10',
            failedChecks: '5',
            consecutiveSuccesses: '0',
            consecutiveFailures: '3',
            lastFailure: 'connect_failed',
          },
        ],
      },
    ],
    rtmp: {
      activeStreams: 3,
      publishers: 2,
      subscribers: 24,
      mediaPayloadBytesReceived: 805_306_368,
    },
  }
}

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), { status })
}

function deferred<T>(): {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
} {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('monitoring dashboard', () => {
  it('renders formatted monitoring data and preserves stream controls', async () => {
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/rtmp/streams') return Promise.resolve(jsonResponse(catalog))
      if (url.endsWith('/start')) {
        return Promise.resolve(
          jsonResponse(
            {
              ...catalog.streams[0]!.recorders[0]!,
              phase: { state: 'starting', operation_id: 'operation-1' },
            },
            202,
          ),
        )
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/monitoring',
      expect.objectContaining({ cache: 'no-store', signal: expect.any(AbortSignal) }),
    )
    expect(wrapper.get('.traffic-panel').text()).toContain('42')
    expect(wrapper.get('.traffic-panel').text()).toContain(new Intl.NumberFormat().format(12_345))
    expect(wrapper.get('.host-panel').text()).toContain('0.42')
    expect(wrapper.get('.process-panel').text()).toContain('12.5%')
    expect(wrapper.get('.readout-bar').text()).toContain('75%')
    expect(wrapper.get('.readout-bar').text()).toContain('1d 1h')
    expect(wrapper.get('.rtmp-panel').text()).toContain('768 MB')
    expect(wrapper.get('.listener-section').text()).toContain('HTTP ingress')
    expect(wrapper.get('.listener-section').text()).toContain('127.0.0.1:8080')
    expect(wrapper.get('.listener-section').text()).toContain(
      `14 / ${new Intl.NumberFormat().format(1_000)}`,
    )
    expect(wrapper.get('.pool-section').text()).toContain('web-backends')
    expect(wrapper.get('.pool-section').text()).toContain('Degraded')
    expect(wrapper.get('.pool-section').text()).toContain('1 / 2 endpoints available')
    expect(wrapper.get('.pool-summary').text()).toContain(
      `${new Intl.NumberFormat().format(BigInt(exactHealthCounter))} unavailable selections`,
    )
    expect(wrapper.get('.endpoint-checks').text()).toContain(
      `${new Intl.NumberFormat().format(BigInt(exactHealthCounter))} passed / 1 failed`,
    )
    expect(wrapper.get('.endpoint-streak').text()).toContain('4 passed / 0 failed')
    expect(wrapper.get('.endpoint-failure').text()).toContain('Last failure: Connection failed')

    expect(wrapper.text()).toContain('live / camera')
    expect(wrapper.text()).toContain('12 viewers')
    expect(wrapper.text()).toContain('AAC')
    expect(wrapper.text()).toContain('AVC')
    await wrapper.get('[data-recorder-action]').trigger('click')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/rtmp/streams/2a130dea-5db7-43e0-afb8-f07c4bcb1814/recorders/c76ad8c2-e575-4989-8fae-1a95566ff598/start',
      expect.objectContaining({ method: 'POST' }),
    )
    wrapper.unmount()
  })

  it('announces the first load without presenting placeholder data as telemetry', async () => {
    const pendingMonitoring = deferred<Response>()
    const pendingCatalog = deferred<Response>()
    const fetch = vi.fn((input: RequestInfo | URL) =>
      String(input) === '/api/v1/monitoring'
        ? pendingMonitoring.promise
        : pendingCatalog.promise,
    )
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await wrapper.vm.$nextTick()

    expect(wrapper.attributes('aria-busy')).toBe('true')
    expect(wrapper.get('.loading-state').attributes('role')).toBe('status')
    expect(wrapper.get('.loading-state').text()).toContain('Establishing telemetry')
    expect(wrapper.find('.monitoring-overview').exists()).toBe(false)

    pendingMonitoring.resolve(jsonResponse(monitoringSample()))
    pendingCatalog.resolve(jsonResponse(catalog))
    await flushPromises()

    expect(wrapper.attributes('aria-busy')).toBe('false')
    expect(wrapper.find('.loading-state').exists()).toBe(false)
    wrapper.unmount()
  })

  it('announces an initial monitoring failure as an error', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) =>
      Promise.resolve(
        String(input) === '/api/v1/monitoring'
          ? jsonResponse({ error: { message: 'metrics offline' } }, 503)
          : jsonResponse(catalog),
      ),
    )
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('.error-notice').attributes('role')).toBe('alert')
    expect(wrapper.get('.error-notice').text()).toContain('Monitoring unavailable')
    expect(wrapper.get('.error-notice').text()).toContain('metrics offline')
    expect(wrapper.find('.monitoring-overview').exists()).toBe(false)
    wrapper.unmount()
  })

  it('renders an explicit empty state when no upstream pools are configured', async () => {
    const sample = monitoringSample()
    sample.upstreamPools = []
    const fetch = vi.fn((input: RequestInfo | URL) =>
      Promise.resolve(
        String(input) === '/api/v1/monitoring'
          ? jsonResponse(sample)
          : jsonResponse(catalog),
      ),
    )
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('.pool-section').text()).toContain('No upstream pools are configured.')
    wrapper.unmount()
  })

  it('keeps the last valid sample and skips periodic refreshes while one is in flight', async () => {
    vi.useFakeTimers()
    const pendingMonitoring = deferred<Response>()
    let monitoringRequests = 0
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') {
        monitoringRequests += 1
        return monitoringRequests === 1
          ? Promise.resolve(jsonResponse(monitoringSample()))
          : pendingMonitoring.promise
      }
      return Promise.resolve(jsonResponse(catalog))
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await vi.advanceTimersByTimeAsync(0)

    await vi.advanceTimersByTimeAsync(5_000)
    await vi.advanceTimersByTimeAsync(15_000)
    expect(monitoringRequests).toBe(2)

    pendingMonitoring.reject(new Error('gateway timeout'))
    await vi.advanceTimersByTimeAsync(0)

    expect(wrapper.get('.traffic-panel').text()).toContain('42')
    expect(wrapper.get('.stale-notice').attributes('role')).toBe('status')
    expect(wrapper.get('.stale-notice').text()).toContain('Retaining the last valid sample')
    expect(wrapper.get('.stale-notice').text()).toContain('gateway timeout')
    expect(wrapper.get('.system-state').text()).toContain('Telemetry stale')
    expect(wrapper.get('.pool-section').text()).toContain('web-backends')
    wrapper.unmount()
  })
})
