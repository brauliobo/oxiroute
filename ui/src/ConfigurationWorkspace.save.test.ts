import { flushPromises } from './test/mount'
import { describe, expect, it, vi } from 'vitest'

import type { CanonicalConfig, ConfigDiagnostic } from './config'
import { defaultHttpCachePolicy } from './configuration/canonicalDefaults'
import { pendingConfirm, settleConfirm } from './shell/confirm'
import { jsonResponse } from './test/contractFixtures'
import { deferred } from './test/deferred'
import {
  activeRevision,
  bearerToken,
  canonicalConfig,
  configSnapshot,
  diskRevision,
  findButton,
  installConfigFetch,
  invalidDiagnostic,
  mountUnlocked,
  restartReason,
  validationResponse,
} from './test/configurationWorkspace'

describe('ConfigurationWorkspace', () => {
  it('validates, presents the format-neutral preview, and saves with If-Config-Revision', async () => {
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked(true)

    await findButton(wrapper, 'https').trigger('click')
    await wrapper.get('[data-field="listeners[].name"] input').setValue('public-https')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    expect(validationCall?.[1]).toEqual(expect.objectContaining({
      method: 'POST',
      headers: expect.objectContaining({ Authorization: `Bearer ${bearerToken}` }),
    }))
    expect(JSON.parse(String(validationCall?.[1]?.body)).config.listeners[0].name).toBe('public-https')
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
    expect(wrapper.get('.preview-panel pre').text()).toContain('version 1')
    expect(wrapper.get('.candidate-topology').text()).toContain('public-https')
    expect(wrapper.get('.candidate-state').text()).toContain('Candidate only / not active')

    const reviewButton = findButton(wrapper, 'Review save')
    ;(reviewButton.element as HTMLButtonElement).focus()
    await reviewButton.trigger('click')
    expect(wrapper.get('dialog[open]').text()).toContain('Save review')
    expect(wrapper.get('.review-warning').text()).toContain('no process restart is required')
    ;(wrapper.get('dialog').element as HTMLDialogElement).close()
    await flushPromises()
    expect(wrapper.find('dialog[open]').exists()).toBe(false)

    await reviewButton.trigger('click')
    expect(wrapper.find('dialog[open]').exists()).toBe(true)
    await wrapper.get('dialog').trigger('click')
    expect(wrapper.find('dialog[open]').exists()).toBe(false)

    await reviewButton.trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    const saveCall = fetch.mock.calls.find(([, init]) => init?.method === 'PUT')
    expect(saveCall?.[1]?.headers).toEqual(expect.objectContaining({
      Authorization: `Bearer ${bearerToken}`,
      'If-Config-Revision': diskRevision,
    }))
    const saved = JSON.parse(String(saveCall?.[1]?.body)).config as CanonicalConfig
    expect(saved.listeners[0]?.bind).toEqual({ type: 'socket', address: '0.0.0.0:443' })
    expect(saved.listeners[2]).toHaveProperty('max_connections', null)
    expect(saved.upstream_pools[0]?.servers).toEqual([
      { name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' },
      { name: 'server-2', endpoint: { type: 'dns', host: 'backend.example.test', port: 3001 }, max_connections: null, dns_resolution: 'on_connect' },
    ])
    expect(saved.http_services[0]).toHaveProperty('max_request_body_bytes', 10_485_760)
    expect(wrapper.get('.revision-banner.pending').text()).toContain('activation pending')
    expect(wrapper.get('.revision-banner.pending').text()).toContain('no restart is required')
    expect(wrapper.find('dialog[open]').exists()).toBe(false)
    expect(document.activeElement).toBe(wrapper.get('.revision-banner.save-state').element)
    wrapper.unmount()
  })

  it('renders an exact unchanged-active 200 outcome without requiring restart', async () => {
    installConfigFetch(() => jsonResponse({
      diskRevision,
      candidateRevision: 'candidate-unchanged',
      activeRevision,
      outcome: 'unchanged_active',
      activationState: 'active',
      restartRequired: false,
      restartReasons: [],
      diagnostics: [],
    }))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    expect(wrapper.get('.review-warning').text()).toContain('no process restart is required')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.success').text()).toContain('unchanged')
    expect(wrapper.get('.revision-banner.success').text()).toContain('No restart is required')
    expect(wrapper.text()).not.toContain('activation pending')
  })

  it('renders the backend explanation for a restart-required outcome', async () => {
    installConfigFetch(() => jsonResponse({
      diskRevision: 'disk-restart-required',
      candidateRevision: 'candidate-restart-required',
      activeRevision,
      outcome: 'saved_restart_required',
      activationState: 'restart_required',
      restartRequired: true,
      restartReasons: [restartReason],
      diagnostics: [{
        code: 'I_RESTART_REQUIRED',
        severity: 'warning',
        stage: 'activation',
        message: 'an active Unix listener mode changed',
      }],
    }), 200, configSnapshot(), true, [{
      code: 'I_RESTART_REQUIRED',
      severity: 'warning',
      stage: 'activation',
      message: 'an active Unix listener mode changed',
    }])
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    const reviewWarning = wrapper.get('.review-warning').text()
    expect(reviewWarning).toContain('active Unix listener mode changed')
    expect(reviewWarning).not.toContain('This active Unix listener mode change')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.pending').text()).toContain('restart required')
    expect(wrapper.get('.revision-banner.pending').text()).toContain('active Unix listener mode')
  })

  it('lists the added and removed listeners of a candidate that reloads without a restart', async () => {
    const listenerChanges: ConfigDiagnostic = {
      code: 'I_LISTENER_CHANGES',
      severity: 'warning',
      stage: 'activation',
      path: '/config/listeners',
      message: 'activation adds analysis (tcp 127.0.0.1:8195) (the supervising master binds them before traffic switches) and removes old (tcp 127.0.0.1:8190) (they stop accepting once the new revision is active and close when their connections finish); every other listener keeps serving without interruption',
    }
    installConfigFetch(() => jsonResponse({
      diskRevision: 'disk-listener-changes',
      candidateRevision: 'candidate-listener-changes',
      activeRevision,
      outcome: 'saved_pending_activation',
      activationState: 'pending',
      restartRequired: false,
      restartReasons: [],
      diagnostics: [listenerChanges, {
        code: 'I_ACTIVATION_PENDING',
        severity: 'warning',
        stage: 'activation',
        message: 'configuration was saved and queued for generation activation',
      }],
    }), 200, configSnapshot(), false, [listenerChanges])
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    const diagnostics = wrapper.get('.diagnostics-section').text()
    expect(diagnostics).toContain('I_LISTENER_CHANGES')
    expect(diagnostics).toContain('adds analysis (tcp 127.0.0.1:8195)')
    expect(diagnostics).toContain('removes old (tcp 127.0.0.1:8190)')
    await findButton(wrapper, 'Review save').trigger('click')
    expect(wrapper.get('.review-warning').text()).toContain('no process restart is required')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.pending').text()).not.toContain('restart required')
  })

  it('renders a supervised topology restart explanation without Unix wording', async () => {
    installConfigFetch(() => jsonResponse({
      diskRevision: 'disk-topology-restart',
      candidateRevision: 'candidate-topology-restart',
      activeRevision,
      outcome: 'saved_restart_required',
      activationState: 'restart_required',
      restartRequired: true,
      restartReasons: [restartReason],
      diagnostics: [{
        code: 'I_RESTART_REQUIRED',
        severity: 'warning',
        stage: 'activation',
        path: '/config/listeners',
        message: 'the supervised listener or control-listener topology changed; the saved configuration takes effect after a process restart',
      }],
    }), 200, configSnapshot(), true, [{
      code: 'I_RESTART_REQUIRED',
      severity: 'warning',
      stage: 'activation',
      path: '/config/listeners',
      message: 'the supervised listener or control-listener topology changed; the saved configuration takes effect after a process restart',
    }])
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    const reviewWarning = wrapper.get('.review-warning').text()
    expect(reviewWarning).toContain('supervised listener or control-listener topology changed')
    expect(reviewWarning).not.toContain('Unix')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    const banner = wrapper.get('.revision-banner.pending').text()
    expect(banner).toContain('supervised listener or control-listener topology changed')
    expect(banner).not.toContain('Unix')
  })

  it('preserves the edited draft after a 409 conflict', async () => {
    installConfigFetch(() => jsonResponse({
      schemaVersion: 1,
      diskRevision: 'disk-newer-revision',
      activeRevision,
      expectedRevision: diskRevision,
      outcome: 'conflict',
      config: canonicalConfig(),
      diagnostics: [],
    }, 409))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'https').trigger('click')
    const name = wrapper.get('[data-field="listeners[].name"] input')
    await name.setValue('preserved-listener')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.stale').text()).toContain('The file on disk changed while you were editing')
    expect(wrapper.get('.revision-banner.stale').text()).toContain('Your draft is kept and nothing was written')
    expect(wrapper.findAll('.revision-banner.error, .revision-banner.save-state')).toHaveLength(0)
    expect((wrapper.get('[data-field="listeners[].name"] input').element as HTMLInputElement).value).toBe('preserved-listener')
    expect(wrapper.find('dialog[open]').exists()).toBe(false)

    await findButton(wrapper, 'Reset draft').trigger('click')
    expect(pendingConfirm.value?.details).toEqual(['/listeners/https', '/listeners/preserved-listener'])
    settleConfirm(false)
    await flushPromises()
    expect(wrapper.get('.revision-banner.stale').element).toBeInstanceOf(HTMLElement)
    expect((wrapper.get('[data-field="listeners[].name"] input').element as HTMLInputElement).value).toBe('preserved-listener')
  })

  it('renders 422 validation diagnostics and keeps review blocked', async () => {
    installConfigFetch(undefined, 422)
    const wrapper = await mountUnlocked(true)

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.diagnostic-list').text()).toContain('E_UNRESOLVED_REFERENCE')
    expect(wrapper.get('.diagnostic-list').text()).toContain('Listener references an unknown HTTP service')
    expect(wrapper.get('.revision-banner.error').text()).toContain('Candidate is invalid')
    expect((findButton(wrapper, 'Review save').element as HTMLButtonElement).disabled).toBe(true)
  })

  it('focuses the forward cache store named by a backend diagnostic', async () => {
    const snapshot = configSnapshot()
    snapshot.config.forward_proxy_services[0]!.cache = defaultHttpCachePolicy('responses')
    const diagnostic: ConfigDiagnostic = {
      code: 'E_UNRESOLVED_REFERENCE',
      severity: 'error',
      stage: 'validation',
      path: 'forward_proxy_services[0].cache.store',
      message: 'Forward cache store does not resolve.',
    }
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(snapshot)
      if (url === '/api/v1/config/validate') {
        const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
        return jsonResponse(validationResponse(body.config, [diagnostic]))
      }
      throw new Error(`Unexpected request: ${url}`)
    }))
    const wrapper = await mountUnlocked(true)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, diagnostic.message).trigger('click')
    await flushPromises()

    const store = wrapper.get('[data-field="forward_proxy_services[].cache.store"] select')
    expect(document.activeElement).toBe(store.element)
  })

  it('aborts and generation-gates superseded validation success and errors', async () => {
    const firstValidation = deferred<Response>()
    const validationSignals: AbortSignal[] = []
    let validationRequests = 0
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config') return jsonResponse(configSnapshot())
      if (url === '/api/v1/config/validate') {
        validationRequests += 1
        validationSignals.push(init?.signal as AbortSignal)
        if (validationRequests === 1) return firstValidation.promise
        const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
        return jsonResponse(validationResponse(body.config))
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await wrapper.get('[data-field="version"] input').setValue(3)
    expect(validationSignals[0]?.aborted).toBe(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    firstValidation.resolve(jsonResponse({ diagnostics: [invalidDiagnostic] }, 422))
    await flushPromises()

    expect(wrapper.get('.preview-panel pre').text()).toContain('version 3')
    expect(wrapper.text()).not.toContain('Candidate is invalid')
    expect(wrapper.find('.diagnostic-list').exists()).toBe(false)
  })

  it('aborts validation when the workspace unmounts', async () => {
    const pendingValidation = deferred<Response>()
    let validationSignal: AbortSignal | undefined
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config') return jsonResponse(configSnapshot())
      if (url === '/api/v1/config/validate') {
        validationSignal = init?.signal as AbortSignal
        return pendingValidation.promise
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    wrapper.unmount()

    expect(validationSignal?.aborted).toBe(true)
  })

  it('keeps save-time 422 diagnostics attached to the rejected candidate', async () => {
    installConfigFetch(() => jsonResponse({
      diagnostics: [invalidDiagnostic],
    }, 422))
    const wrapper = await mountUnlocked(true)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.error').text()).toContain('Save rejected as invalid')
    expect(wrapper.get('.diagnostic-list').text()).toContain('E_UNRESOLVED_REFERENCE')
    expect(wrapper.find('.output-empty').exists()).toBe(false)
    expect(document.activeElement).toBe(wrapper.get('#diagnostics-heading').element)
  })

  it('keeps the review dialog visible for a 428 revision precondition failure', async () => {
    installConfigFetch(() => jsonResponse({
      error: { code: 'precondition_required', message: 'configuration revision required' },
    }, 428))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('dialog[open]').text()).toContain('revision precondition')
    expect(wrapper.get('.revision-banner.stale').element).toBeInstanceOf(HTMLElement)
    expect(wrapper.get('dialog .ox-primary').attributes()).toHaveProperty('disabled')
  })

  it('blocks writes when the server cannot reload authoritative state after a conflict', async () => {
    installConfigFetch(() => jsonResponse({
      schemaVersion: 1,
      diskRevision: null,
      activeRevision,
      expectedRevision: diskRevision,
      outcome: 'authoritative_state_unavailable',
      diagnostics: [{
        code: 'E_CONFIG_READ',
        severity: 'error',
        stage: 'read',
        message: 'canonical configuration could not be read',
      }],
      error: {
        code: 'authoritative_config_unavailable',
        message: 'the latest persisted configuration could not be loaded',
      },
    }, 503))
    const wrapper = await mountUnlocked()

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('.revision-banner.stale').text()).toContain('a revision that could not be read')
    expect(wrapper.findAll('.revision-banner.error, .revision-banner.save-state')).toHaveLength(0)
    expect(wrapper.get('.diagnostic-list').text()).toContain('E_CONFIG_READ')
    expect(wrapper.find('dialog[open]').exists()).toBe(false)
  })

  it.each([
    {
      outcome: 'write_failed',
      diskRevision,
      code: 'E_CONFIG_TEMP_WRITE',
      expected: 'canonical write failed',
    },
  ] as const)('keeps 500 $outcome details visible in the review dialog', async ({
    outcome,
    diskRevision: failureRevision,
    code,
    expected,
  }) => {
    installConfigFetch(() => jsonResponse({
      diskRevision: failureRevision,
      activeRevision,
      outcome,
      diagnostics: [{
        code,
        severity: 'error',
        stage: 'write',
        message: 'The canonical persistence operation failed.',
      }],
    }, 500))
    const wrapper = await mountUnlocked()

    await wrapper.get('[data-field="version"] input').setValue(2)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, 'Review save').trigger('click')
    await findButton(wrapper, 'Save canonical configuration').trigger('click')
    await flushPromises()

    expect(wrapper.get('dialog .dialog-error').text()).toContain(expected)
    expect(wrapper.get('.diagnostic-list').text()).toContain(code)
    expect(wrapper.get('.revision-board').text()).toContain('Unsaved changes')
    expect(wrapper.find('dialog[open]').exists()).toBe(true)
  })
})
