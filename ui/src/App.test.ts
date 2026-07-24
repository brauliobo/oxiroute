import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'

import App from './App.vue'
import {
  contractCatalog,
  contractMonitoring,
  contractTopology,
  jsonResponse,
} from './test/contractFixtures'

const exactHealthCounter = '18446744073709551615'
const catalog = contractCatalog()
const topology = contractTopology()

function monitoringSample() {
  return contractMonitoring()
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
  it('visibly distinguishes listening, failed, and stopped listeners', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
      return Promise.resolve(jsonResponse(catalog))
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('.listener-list').text()).toContain('Listening')
    expect(wrapper.get('.listener-failed').text()).toContain('Live edge')
    expect(wrapper.get('.listener-failed .listener-state').text()).toBe('Failed')
    expect(wrapper.get('.listener-stopped').text()).toContain('Forward H3')
    expect(wrapper.get('.listener-stopped .listener-state').text()).toBe('Stopped')
    wrapper.unmount()
  })

  it('renders formatted monitoring data and preserves stream controls', async () => {
    const runtimeCatalog = structuredClone(catalog)
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/rtmp/streams') return Promise.resolve(jsonResponse(runtimeCatalog))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
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
    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/topology',
      expect.objectContaining({ cache: 'no-store', signal: expect.any(AbortSignal) }),
    )
    expect(wrapper.get('.topology-section').text()).toContain('Network topology')
    expect(wrapper.get('.topology-section').text()).toContain('127.0.0.1:3000')
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
    expect(wrapper.get('.listener-section').text()).toContain('28 / Unbounded')
    expect(wrapper.get('.pool-section').text()).toContain('web-backends')
    expect(wrapper.get('.pool-section').text()).toContain('Least connections')
    expect(wrapper.get('.pool-section').text()).toContain('Active leases: 3')
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
    expect(wrapper.get('.recorder-panel').text()).toContain('Recording supported')
    expect(wrapper.get('.recorder-panel').text()).toContain('Manual')
    expect(wrapper.get('.recorder-panel').text()).toContain('Continuous')
    expect(wrapper.get('.recorder-panel').text()).toContain('1.0 MB written')
    expect(wrapper.get('.recorder-panel').text()).toContain('2 segments')
    expect(wrapper.get('.recorder-panel').text()).toContain('1 discontinuity')
    expect(wrapper.get('.recorder-panel').text()).toContain('live/camera-001.flv')
    expect(wrapper.get('.recorder-panel').text()).toContain('live/.camera-002.partial')
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
    const pendingTopology = deferred<Response>()
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return pendingMonitoring.promise
      if (url === '/api/v1/rtmp/streams') return pendingCatalog.promise
      return pendingTopology.promise
    })
    vi.stubGlobal('fetch', fetch)

    const wrapper = mount(App)
    await wrapper.vm.$nextTick()

    expect(wrapper.attributes('aria-busy')).toBe('true')
    expect(wrapper.get('.loading-state').attributes('role')).toBe('status')
    expect(wrapper.get('.loading-state').text()).toContain('Establishing telemetry')
    expect(wrapper.find('.monitoring-overview').exists()).toBe(false)

    pendingMonitoring.resolve(jsonResponse(monitoringSample()))
    pendingCatalog.resolve(jsonResponse(catalog))
    pendingTopology.resolve(jsonResponse(topology))
    await flushPromises()

    expect(wrapper.attributes('aria-busy')).toBe('false')
    expect(wrapper.find('.loading-state').exists()).toBe(false)
    wrapper.unmount()
  })

  it('renders settled telemetry while another overview resource is still loading', async () => {
    const pendingCatalog = deferred<Response>()
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
      return pendingCatalog.promise
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('.traffic-panel').text()).toContain('42')
    expect(wrapper.get('.topology-section').text()).toContain('Network topology')
    expect(wrapper.get('.loading-notice').text()).toContain('Loading stream inventory')
    wrapper.unmount()
  })

  it('cancels an in-flight overview refresh and starts a fresh round when returning', async () => {
    const pending = deferred<Response>()
    const firstSignals: AbortSignal[] = []
    const requestCounts = new Map<string, number>()
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      const count = (requestCounts.get(url) ?? 0) + 1
      requestCounts.set(url, count)
      if (count === 1) {
        firstSignals.push(init?.signal as AbortSignal)
        return pending.promise
      }
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/rtmp/streams') return Promise.resolve(jsonResponse(catalog))
      return Promise.resolve(jsonResponse(topology))
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await wrapper.vm.$nextTick()

    await wrapper.get('a[href="#/configuration"]').trigger('click')
    expect(firstSignals).toHaveLength(3)
    expect(firstSignals.every((signal) => signal.aborted)).toBe(true)

    await wrapper.get('a[href="#/overview"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('.traffic-panel').text()).toContain('42')
    expect(requestCounts.get('/api/v1/monitoring')).toBe(2)
    wrapper.unmount()
  })

  it('sends an exact stop for a manual recorder already recording', async () => {
    const recordingCatalog = structuredClone(catalog)
    recordingCatalog.streams[0]!.recorders[0]!.phase = {
      state: 'recording',
      operation_id: 'operation-recording',
      started_at_unix_ms: 1_750_000_000_000,
    }
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/rtmp/streams') return Promise.resolve(jsonResponse(recordingCatalog))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
      if (url.endsWith('/stop')) {
        return Promise.resolve(jsonResponse({
          ...recordingCatalog.streams[0]!.recorders[0]!,
          phase: { state: 'stopping', operation_id: 'operation-stop' },
        }, 202))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()

    expect(wrapper.get('[data-recorder-action]').text()).toBe('Stop recording')
    await wrapper.get('[data-recorder-action]').trigger('click')
    await flushPromises()

    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/rtmp/streams/2a130dea-5db7-43e0-afb8-f07c4bcb1814/recorders/c76ad8c2-e575-4989-8fae-1a95566ff598/stop',
      expect.objectContaining({ method: 'POST' }),
    )
    wrapper.unmount()
  })

  it.each([
    [501, 'rtmp_recording_unavailable', 'manual recording is unavailable in the active runtime', 'Manual recording is unavailable'],
    [404, 'rtmp_resource_not_found', 'recorder does not exist', 'target stream or recorder no longer exists'],
    [409, 'rtmp_state_conflict', 'opposite transition in progress', 'state changed before the command completed'],
    [503, 'rtmp_recorder_start_failed', 'the recorder could not be started', 'Recorder command failed. The recorder could not be started'],
  ])('handles stable recorder error %s without claiming success', async (status, code, message, expected) => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/rtmp/streams') return Promise.resolve(jsonResponse(catalog))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
      if (url.endsWith('/start')) {
        return Promise.resolve(jsonResponse({ error: { code, message } }, status))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()

    await wrapper.get('[data-recorder-action]').trigger('click')
    await flushPromises()

    expect(wrapper.get('.error-notice').text()).toContain(expected)
    expect(wrapper.get('.error-notice').text().toLowerCase()).not.toContain('success')
    wrapper.unmount()
  })

  it('detects a replacement stream while a recorder command is in flight', async () => {
    const pendingCommand = deferred<Response>()
    const replacement = structuredClone(catalog)
    replacement.streams[0]!.id = 'replacement-stream-id'
    replacement.streams[0]!.revision = '1'
    let catalogRequests = 0
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(monitoringSample()))
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
      if (url === '/api/v1/rtmp/streams') {
        catalogRequests += 1
        return Promise.resolve(jsonResponse(catalogRequests === 1 ? catalog : replacement))
      }
      if (url.endsWith('/start')) return pendingCommand.promise
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = mount(App)
    await flushPromises()

    await wrapper.get('[data-recorder-action]').trigger('click')
    await wrapper.get('.refresh-button').trigger('click')
    await flushPromises()
    pendingCommand.resolve(jsonResponse({
      ...catalog.streams[0]!.recorders[0]!,
      phase: { state: 'starting', operation_id: 'stale-operation' },
    }, 202))
    await flushPromises()

    expect(wrapper.get('.error-notice').text()).toContain('publisher stream was replaced')
    expect(wrapper.get('.error-notice').text()).toContain('no command success was assumed')
    expect(wrapper.text()).toContain('live / camera')
    wrapper.unmount()
  })

  it('announces an initial monitoring failure as an error', async () => {
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') {
        return Promise.resolve(jsonResponse({ error: { message: 'metrics offline' } }, 503))
      }
      return Promise.resolve(jsonResponse(url === '/api/v1/topology' ? topology : catalog))
    })
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
    const fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input)
      if (url === '/api/v1/monitoring') return Promise.resolve(jsonResponse(sample))
      return Promise.resolve(jsonResponse(url === '/api/v1/topology' ? topology : catalog))
    })
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
      if (url === '/api/v1/topology') return Promise.resolve(jsonResponse(topology))
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
