import { afterEach, describe, expect, it, vi } from 'vitest'
import '../test/dialog'
import { flushPromises, mount, type VaporWrapper } from '../test/mount'
import { clearToasts, toasts } from '#design/toast/state'
import OperationsWorkspace from '../OperationsWorkspace.vue'
import ConfirmHost from '../shell/ConfirmHost.vue'
import { pendingConfirm, settleConfirm } from '../shell/confirm'
import { jsonResponse } from '../test/contractFixtures'
import { managementGeneration, managementStatus } from '../test/managementFixtures'
import { inflightList } from '../test/operationsFixtures'
import { operationsFetch, type RecordedCall } from '../test/operationsFetch'

const token = 'operations-test-token'
const reloadBody = { expectedActiveRevision: 'active-revision' }

afterEach(() => {
  settleConfirm(false)
  clearToasts()
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

async function open(reply?: (call: RecordedCall) => Response | undefined) {
  const net = operationsFetch(reply)
  vi.stubGlobal('fetch', net.fetch)
  const wrapper = mount(OperationsWorkspace, { props: { token } })
  await flushPromises()
  return { wrapper, ...net }
}

async function openTab(wrapper: VaporWrapper, label: string): Promise<void> {
  const tab = wrapper.findAll('[role="tab"]').find((candidate) => candidate.text() === label)
  await tab!.trigger('click')
  await flushPromises()
}

async function answer(confirmed: boolean) {
  await flushPromises()
  const asked = pendingConfirm.value
  settleConfirm(confirmed)
  await flushPromises()
  return asked
}

const reloadsAfter = (calls: RecordedCall[], mutation: RecordedCall) =>
  calls.slice(calls.indexOf(mutation) + 1).filter((call) => call.method === 'GET' && call.url === '/api/v1/status')

const feedback = (wrapper: VaporWrapper) => wrapper.get('[data-feedback]')

describe('operations workspace loading', () => {
  it('shows an authentication error without creating operational placeholder state', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(jsonResponse({
      error: { code: 'unauthorized', message: 'invalid bearer token' },
    }, 401))))
    const wrapper = mount(OperationsWorkspace, { props: { token } })
    await flushPromises()

    expect(wrapper.get('[role="alert"]').text()).toContain('invalid bearer token')
    expect(wrapper.find('[role="tablist"]').exists()).toBe(false)
    expect(wrapper.emitted('unauthorized')).toHaveLength(1)
  })

  it('asks for the token and requests nothing without one', async () => {
    const net = operationsFetch()
    vi.stubGlobal('fetch', net.fetch)
    const wrapper = mount(OperationsWorkspace, { props: { token: '' } })
    await flushPromises()

    expect(wrapper.text()).toContain('Management token required')
    expect(net.calls).toHaveLength(0)
  })

  it('reloads on demand and never polls', async () => {
    const { wrapper, calls } = await open()
    expect(calls.filter((call) => call.url === '/api/v1/status')).toHaveLength(1)

    await wrapper.get('[data-action="refresh-operations"]').trigger('click')
    await flushPromises()
    expect(calls.filter((call) => call.url === '/api/v1/status')).toHaveLength(2)
  })

  it('shows the restart indicator with its reasons', async () => {
    const reasons = [{ code: 'listener_ipv6_only', path: '/listeners/web', summary: 'ipv6_only changed on a kept address' }]
    const { wrapper } = await open((call) => {
      if (call.url !== '/api/v1/status') return undefined
      return jsonResponse({
        ...managementStatus(),
        restartRequired: { required: true, candidateRevision: 'next', since: 1_750_000_000_000, reasons },
      })
    })

    const banner = wrapper.get('[data-restart-reasons]')
    expect(wrapper.text()).toContain('Restart required.')
    expect(wrapper.text()).toContain('Waiting since 2025-06-15 15:06:40 UTC')
    expect(banner.text()).toContain('/listeners/web')
    expect(banner.text()).toContain('ipv6_only changed on a kept address')
  })

  it('shows no restart banner when nothing waits for a restart', async () => {
    const { wrapper } = await open()
    expect(wrapper.find('[data-restart-reasons]').exists()).toBe(false)
  })

  it('lists the capabilities and the supervision mode', async () => {
    const { wrapper } = await open()
    const rows = wrapper.findAll('.ox-ops-capabilities tbody tr').map((row) => row.text())

    expect(rows).toHaveLength(4)
    expect(rows[0]).toContain('Supervision')
    expect(rows[0]).toContain('Direct')
    expect(rows[2]).toContain('HTTP/3 reverse')
    expect(rows[2]).toContain('Not configured')
  })

  it('shows a waiting reload with the connections that hold it back', async () => {
    const { wrapper } = await open((call) => {
      if (call.url !== '/api/v1/generations') return undefined
      const response = managementGeneration()
      response.generation.pendingApply = { revision: 'next-revision', reason: 'retired_draining', connections: 3 }
      return jsonResponse(response)
    })

    const pending = wrapper.get('[data-pending-apply]').text()
    expect(pending).toContain('A reload is waiting.')
    expect(pending).toContain('the previous worker is still draining')
    expect(pending).toContain('3 connections remain')
  })
})

describe('administrative state', () => {
  it('shows the state of listeners and pools as a word', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Listeners and pools')

    const listener = wrapper.get('[data-listener="Live edge"]').text()
    expect(listener).toContain('Maintenance')
    expect(listener).toContain('Failed')
    expect(wrapper.get('[data-listener="Forward H3"]').text()).toContain('Draining')
    expect(wrapper.get('[data-pool="web-backends"]').text()).toContain('Mixed: 1 of 2 not ready')
    expect(wrapper.get('[data-pool="static"]').text()).toContain('Ready')
    expect(wrapper.get('[data-listener="Live edge"] [data-state="maintenance"]').attributes('aria-pressed')).toBe('true')
  })
})

describe('feedback that stays', () => {
  it('keeps the outcome and the persistence note after the reload that follows the action', async () => {
    const { wrapper, calls, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[data-server="primary"] [data-state="drain"]').trigger('click')
    await answer(true)

    const mutation = mutations()[0]!
    expect(reloadsAfter(calls, mutation)).toHaveLength(1)
    const status = feedback(wrapper).get('[role="status"]')
    expect(status.text()).toContain('Server primary in pool web-backends is now draining.')
    expect(status.text()).toContain('Runtime state only')
    expect(feedback(wrapper).get('[role="alert"]').text()).toBe('')
    expect(toasts.value.map((toast) => toast.text)).toEqual(['Server primary in pool web-backends is now draining.'])
  })

  it('says what failed for which target and reloads when the revision was stale', async () => {
    const { wrapper, calls, mutations } = await open((call) =>
      call.method === 'POST'
        ? jsonResponse({ error: { code: 'generation_conflict', message: 'the active generation revision changed' } }, 409)
        : undefined,
    )
    await openTab(wrapper, 'Servers')

    await wrapper.get('[data-server="primary"] [data-health="down"]').trigger('click')
    await answer(true)

    const alert = feedback(wrapper).get('[role="alert"]').text()
    expect(alert).toContain('Could not set the health of server primary in pool web-backends to forced down')
    expect(alert).toContain('the active generation revision changed')
    expect(alert).toContain('reloaded')
    expect(reloadsAfter(calls, mutations()[0]!)).toHaveLength(1)
    expect(toasts.value[0]!.tone).toBe('bad')
  })

  it('keeps a server failure visible without reloading', async () => {
    const { wrapper, calls, mutations } = await open((call) =>
      call.method === 'PUT' ? jsonResponse({ error: { code: 'invalid_weight', message: 'weight must be between 1 and 100' } }, 400) : undefined,
    )
    await openTab(wrapper, 'Servers')
    await wrapper.get('[aria-label="Weight of server primary in pool web-backends"]').setValue('40')
    await wrapper.get('[data-server="primary"] .ox-ops-form:last-child').trigger('submit')
    await flushPromises()

    expect(feedback(wrapper).get('[role="alert"]').text()).toContain('Could not set the weight of server primary')
    expect(reloadsAfter(calls, mutations()[0]!)).toHaveLength(0)
  })

  it('replaces the previous outcome with the new one and clears it on dismissal', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Servers')
    await wrapper.get('[data-server="primary"] [data-state="ready"]').trigger('click')
    await flushPromises()
    expect(feedback(wrapper).text()).toContain('Server primary in pool web-backends is now ready.')

    await wrapper.get('[data-server="static-1"] [data-state="ready"]').trigger('click')
    await flushPromises()
    expect(feedback(wrapper).text()).toContain('Server static-1 in pool static is now ready.')
    expect(feedback(wrapper).text()).not.toContain('Server primary')

    const dismiss = feedback(wrapper).findAll('button').find((button) => button.text() === 'Dismiss')
    await dismiss!.trigger('click')
    expect(feedback(wrapper).text()).toBe('')
  })

  it('shows the outcome next to the action that produced it', async () => {
    const { wrapper } = await open()
    await wrapper.get('[data-generation-action="reload"]').trigger('click')
    await answer(true)
    expect(feedback(wrapper).text()).toContain('Reload requested')

    await openTab(wrapper, 'Servers')
    expect(feedback(wrapper).text()).toBe('')
  })
})

describe('confirmation before destructive actions', () => {
  it.each([
    ['[data-generation-action="reload"]', 'Reload the persisted configuration', 'Serving now'],
    ['[data-generation-action="rollback"]', 'Roll back to the previous generation', 'Rolls back to'],
    ['[data-generation-action="drain"]', 'Drain the active generation', 'Active connections now: 42'],
    ['[data-process-action="drain"]', 'Drain the process', 'Active connections now: 42'],
    ['[data-process-action="shutdown"]', 'Shut down the process', 'Active connections now: 42'],
  ])('asks before %s', async (selector, title, detail) => {
    const { wrapper, mutations } = await open()
    await wrapper.get(selector).trigger('click')
    await flushPromises()

    expect(pendingConfirm.value?.title).toBe(title)
    expect(pendingConfirm.value?.details?.join(' ')).toContain(detail)
    expect(mutations()).toHaveLength(0)
  })

  it('needs the typed phrase for the shutdown', async () => {
    const { wrapper } = await open()
    await wrapper.get('[data-process-action="shutdown"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.typeToConfirm).toBe('shutdown')
    expect(pendingConfirm.value?.danger).toBe(true)
  })

  it('names the listener, pool and server and the live impact', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Listeners and pools')

    await wrapper.get('[data-listener="HTTP ingress"] [data-state="drain"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.title).toBe('Drain listener HTTP ingress')
    expect(pendingConfirm.value?.details).toContain('Active connections now: 14')
    settleConfirm(false)

    await wrapper.get('[data-pool="web-backends"] [data-state="maintenance"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.title).toBe('Put in maintenance pool web-backends')
    expect(pendingConfirm.value?.details).toContain('2 servers')
    expect(pendingConfirm.value?.details).toContain('Active connections now: 3')
    settleConfirm(false)

    await openTab(wrapper, 'Servers')
    await wrapper.get('[data-server="primary"] [data-health="down"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.title).toBe('Force server primary down')
    expect(pendingConfirm.value?.details).toContain('Pool: web-backends')
  })

  it('sends nothing when the dialog is cancelled and nothing is reported', async () => {
    const { wrapper, mutations, calls } = await open()
    await openTab(wrapper, 'Servers')
    const before = calls.length

    await wrapper.get('[data-server="primary"] [data-state="maintenance"]').trigger('click')
    await answer(false)

    expect(mutations()).toHaveLength(0)
    expect(calls).toHaveLength(before)
    expect(feedback(wrapper).text()).toBe('')
    expect(toasts.value).toHaveLength(0)
  })

  it('sends the exact request when the dialog is confirmed', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[data-server="primary"] [data-state="maintenance"]').trigger('click')
    await answer(true)

    expect(mutations()).toEqual([{
      url: '/api/v1/servers/administrative-state',
      method: 'POST',
      body: { targets: [{ pool: 'web-backends', server: 'primary' }], state: 'maintenance', ...reloadBody },
    }])
  })

  it('restores a server without asking', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[data-server="secondary"] [data-state="ready"]').trigger('click')
    await flushPromises()

    expect(pendingConfirm.value).toBeNull()
    expect(mutations()).toHaveLength(1)
  })

  it('drives the real dialog: confirm sends the request, cancel does not', async () => {
    const host = mount(ConfirmHost)
    const { wrapper, mutations } = await open()

    await wrapper.get('[data-generation-action="rollback"]').trigger('click')
    await flushPromises()
    expect(host.attributes('open')).toBeDefined()
    expect(host.get('h2').text()).toBe('Roll back to the previous generation')
    await host.get('[data-close]').trigger('click')
    await flushPromises()
    expect(mutations()).toHaveLength(0)

    await wrapper.get('[data-generation-action="rollback"]').trigger('click')
    await flushPromises()
    await host.get('[data-confirm]').trigger('click')
    await flushPromises()
    expect(mutations().map((call) => call.url)).toEqual(['/api/v1/generations/rollback'])
  })
})

describe('server weight', () => {
  it('sends the weight with the revision and shows the outcome', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[aria-label="Weight of server primary in pool web-backends"]').setValue('40')
    await wrapper.get('[data-server="primary"] .ox-ops-form:last-child').trigger('submit')
    await flushPromises()

    expect(mutations()).toEqual([{
      url: '/api/v1/servers/weight',
      method: 'PUT',
      body: { targets: [{ pool: 'web-backends', server: 'primary' }], weight: 40, ...reloadBody },
    }])
    expect(feedback(wrapper).text()).toContain('Weight of server primary in pool web-backends is 40.')
  })

  it('resets the weight with null', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[aria-label="Reset weight for server primary in pool web-backends"]').trigger('click')
    await flushPromises()

    expect(mutations()[0]!.body).toEqual({ targets: [{ pool: 'web-backends', server: 'primary' }], weight: null, ...reloadBody })
    expect(feedback(wrapper).text()).toContain('back to its configured value')
  })

  it('refuses a weight outside 1 to 100 before sending anything', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[aria-label="Weight of server primary in pool web-backends"]').setValue('101')
    await flushPromises()

    expect(wrapper.get('[data-server="primary"]').text()).toContain('Enter a whole number from 1 to 100.')
    const apply = wrapper.get('[aria-label="Apply weight for server primary in pool web-backends"]')
    expect(apply.attributes('disabled')).toBeDefined()
    expect(mutations()).toHaveLength(0)
  })

  it('sets and resets the connection limit', async () => {
    const { wrapper, mutations } = await open()
    await openTab(wrapper, 'Servers')

    await wrapper.get('[aria-label="Max connections of server primary in pool web-backends"]').setValue('250')
    await wrapper.get('[data-server="primary"] .ox-ops-form').trigger('submit')
    await flushPromises()
    await wrapper.get('[aria-label="Reset limit for server primary in pool web-backends"]').trigger('click')
    await flushPromises()

    expect(mutations().map((call) => [call.method, call.url, (call.body as { maxConnections: number | null }).maxConnections])).toEqual([
      ['PUT', '/api/v1/servers/max-connections', 250],
      ['PUT', '/api/v1/servers/max-connections', null],
    ])
  })
})

describe('evacuation', () => {
  const report = { outcome: 'evacuated', force: true, cancelledAndRedispatched: 2, forced: 1, leftRunning: { no_destination: 3 } }

  it('evacuates a server with the force toggle and surfaces what was left running', async () => {
    const { wrapper, mutations } = await open((call) => (call.url.endsWith('/evacuate') ? jsonResponse(report) : undefined))
    await openTab(wrapper, 'Servers')
    await wrapper.get('.ox-ops-toolbar input[type="checkbox"]').setValue(true)

    await wrapper.get('[data-server="primary"] [data-action="evacuate"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.title).toBe('Evacuate server primary')
    expect(pendingConfirm.value?.details).toContain('Force: on')
    settleConfirm(true)
    await flushPromises()

    expect(mutations()).toEqual([{
      url: '/api/v1/servers/evacuate',
      method: 'POST',
      body: { targets: [{ pool: 'web-backends', server: 'primary' }], force: true, ...reloadBody },
    }])
    const text = feedback(wrapper).get('[role="status"]').text()
    expect(text).toContain('2 requests moved to other servers, 1 request cut, 3 requests left running')
    expect(text).toContain('3 requests left running: no other server can take them now.')
  })

  it('does not force unless asked', async () => {
    const { wrapper, mutations } = await open((call) =>
      call.url.endsWith('/evacuate')
        ? jsonResponse({ ...report, force: false, forced: 0, leftRunning: {} })
        : undefined,
    )
    await openTab(wrapper, 'Servers')
    await wrapper.get('[data-server="primary"] [data-action="evacuate"]').trigger('click')
    await answer(true)

    expect((mutations()[0]!.body as { force: boolean }).force).toBe(false)
    expect(feedback(wrapper).get('[role="status"]').text()).toContain('0 requests left running')
  })

  it('evacuates a pool and says that it only drains by default', async () => {
    const { wrapper, mutations } = await open((call) => (call.url.endsWith('/evacuate') ? jsonResponse(report) : undefined))
    await openTab(wrapper, 'Listeners and pools')

    await wrapper.get('[data-pool="web-backends"] [aria-label="Evacuate pool web-backends"]').trigger('click')
    await flushPromises()
    expect(pendingConfirm.value?.title).toBe('Evacuate pool web-backends')
    expect(pendingConfirm.value?.message).toContain('no destination')
    settleConfirm(true)
    await flushPromises()

    expect(mutations()[0]).toEqual({
      url: '/api/v1/pools/evacuate',
      method: 'POST',
      body: { pools: ['web-backends'], force: false, ...reloadBody },
    })
    expect(feedback(wrapper).get('[role="status"]').text()).toContain('Evacuated pool web-backends')
  })
})

describe('server list', () => {
  it('hides the controls that cannot apply', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Servers')
    const buttons = (name: string) => wrapper.get(`[data-server="${name}"]`).findAll('button').map((button) => button.text())

    expect(buttons('primary')).toContain('Disable checks')
    expect(buttons('primary')).not.toContain('Refresh DNS')
    expect(buttons('static-1')).toContain('Refresh DNS')
    expect(buttons('static-1')).not.toContain('Disable checks')
    expect(buttons('static-1')).not.toContain('Enable checks')
    expect(wrapper.get('[data-server="static-1"]').text()).not.toContain('Checks')
  })

  it('searches by name, pool or address', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Servers')
    expect(wrapper.findAll('[data-server]')).toHaveLength(3)

    await wrapper.get('input[type="search"]').setValue('origin.example')
    expect(wrapper.findAll('[data-server]').map((card) => card.attributes('data-server'))).toEqual(['static-1'])
    expect(wrapper.text()).toContain('Showing 1 of 3 servers.')

    await wrapper.get('input[type="search"]').setValue('nothing-like-this')
    expect(wrapper.text()).toContain('No server matches')
  })

  it('shows only the servers with a problem on demand', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Servers')

    const problems = wrapper.findAll('button').find((button) => button.text() === 'Problems only')!
    await problems.trigger('click')

    expect(problems.attributes('aria-pressed')).toBe('true')
    expect(wrapper.findAll('[data-server]').map((card) => card.attributes('data-server'))).toEqual(['secondary'])
  })

  it('names the weight next to the connection limit', async () => {
    const { wrapper } = await open()
    await openTab(wrapper, 'Servers')
    const card = wrapper.get('[data-server="static-1"]').text()

    expect(card).toContain('Weight')
    expect(card).toContain('Max connections')
    expect(card).toContain('unbounded')
  })
})

describe('in-flight requests', () => {
  it('lists the requests oldest first when the tab opens', async () => {
    const { wrapper, calls } = await open()
    await openTab(wrapper, 'In-flight requests')

    expect(calls.at(-1)).toMatchObject({ method: 'GET', url: '/api/v1/requests?limit=100' })
    const rows = wrapper.findAll('[data-request]').map((row) => row.attributes('data-request'))
    expect(rows).toEqual(['rq-old', 'rq-new'])
    expect(wrapper.get('[data-request="rq-old"]').text()).toContain('42 s')
    expect(wrapper.get('[data-request="rq-old"]').text()).toContain('Overdue')
    expect(wrapper.get('[data-request="rq-new"]').text()).toContain('queue position 1')
    expect(wrapper.text()).toContain('Showing 2 of 2 requests in flight, oldest first.')
  })

  it('sends the filters the API defines', async () => {
    const { wrapper, calls } = await open()
    await openTab(wrapper, 'In-flight requests')

    await wrapper.get('select').setValue('web-backends')
    await wrapper.findAll('select')[1]!.setValue('queued')
    await wrapper.get('input[type="number"]').setValue('30')
    await wrapper.get('form.ox-ops-filters').trigger('submit')
    await flushPromises()

    expect(calls.at(-1)!.url).toBe('/api/v1/requests?pool=web-backends&phase=queued&min_age_ms=30000&limit=100')
  })

  it('refuses an invalid limit locally', async () => {
    const { wrapper, calls } = await open()
    await openTab(wrapper, 'In-flight requests')
    const before = calls.length

    await wrapper.findAll('input[type="number"]')[1]!.setValue('5000')
    await wrapper.get('form.ox-ops-filters').trigger('submit')
    await flushPromises()

    expect(calls).toHaveLength(before)
    expect(wrapper.get('.ox-ops-requests [role="alert"]').text()).toContain('from 1 to 1000')
  })

  it('says when the list is truncated and when nothing is in flight', async () => {
    const { wrapper } = await open((call) => {
      if (!call.url.startsWith('/api/v1/requests')) return undefined
      return jsonResponse(call.url.includes('pool=') ? { requests: [], total: '0', truncated: false } : { ...inflightList(), total: '342', truncated: true })
    })
    await openTab(wrapper, 'In-flight requests')
    expect(wrapper.text()).toContain('Showing 2 of 342 requests in flight, oldest first. Narrow the filters')

    await wrapper.get('select').setValue('web-backends')
    await wrapper.get('form.ox-ops-filters').trigger('submit')
    await flushPromises()
    expect(wrapper.text()).toContain('No in-flight request matches the filters.')
  })

  it('reports a failed read without hiding the previous list', async () => {
    let fail = false
    const { wrapper } = await open((call) => {
      if (!call.url.startsWith('/api/v1/requests') || !fail) return undefined
      return jsonResponse({ error: { code: 'invalid_request', message: 'unknown phase' } }, 400)
    })
    await openTab(wrapper, 'In-flight requests')
    fail = true
    await wrapper.get('form.ox-ops-filters').trigger('submit')
    await flushPromises()

    expect(wrapper.get('.ox-ops-requests [role="alert"]').text()).toContain('unknown phase')
    expect(wrapper.findAll('[data-request]')).toHaveLength(2)
  })
})

describe('registrations', () => {
  it('lists the registered servers with their lease and status', async () => {
    const { wrapper, calls } = await open()
    await openTab(wrapper, 'Registrations')

    expect(calls.at(-1)).toMatchObject({ method: 'GET', url: '/api/v1/registrations' })
    const rows = wrapper.findAll('.ox-ops-registrations tbody tr').map((row) => row.text())
    expect(rows).toHaveLength(2)
    expect(rows[0]).toContain('worker-a')
    expect(rows[0]).toContain('Active')
    expect(rows[0]).toContain('expires in 25 s')
    expect(rows[0]).toContain('zone=a')
    expect(rows[1]).toContain('Shadowed')
    expect(rows[1]).toContain('no lease')
    expect(rows[1]).toContain('announced draining')
  })

  it('explains that registration is not configured', async () => {
    const { wrapper } = await open((call) => call.url === '/api/v1/registrations'
      ? jsonResponse({ error: { code: 'registry_disabled', message: 'server registration is not configured' } }, 404)
      : undefined)
    await openTab(wrapper, 'Registrations')

    expect(wrapper.text()).toContain('Server registration is not configured')
    expect(wrapper.find('.ox-ops-registrations [role="alert"] .ox-banner').exists()).toBe(false)
  })

  it('reports a registry failure and a degraded registry', async () => {
    const { wrapper } = await open((call) => call.url === '/api/v1/registrations'
      ? jsonResponse({ error: { code: 'registry_unavailable', message: 'the registry store is unavailable' } }, 503)
      : undefined)
    await openTab(wrapper, 'Registrations')
    expect(wrapper.get('.ox-ops-registrations [role="alert"]').text()).toContain('the registry store is unavailable')
  })

  it('marks a degraded registry', async () => {
    const { wrapper } = await open((call) => call.url === '/api/v1/registrations'
      ? jsonResponse({ registrations: [], registryRevision: 'r', writer: false, degraded: 'store is read only' })
      : undefined)
    await openTab(wrapper, 'Registrations')

    expect(wrapper.text()).toContain('The registry is degraded.')
    expect(wrapper.text()).toContain('store is read only')
    expect(wrapper.text()).toContain('only reads the registry')
    expect(wrapper.text()).toContain('No machine has registered itself')
  })
})
