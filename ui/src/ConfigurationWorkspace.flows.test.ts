import { flushPromises } from './test/mount'
import { describe, expect, it, vi } from 'vitest'

import type { CanonicalConfig } from './config'
import { jsonResponse } from './test/contractFixtures'
import {
  configSnapshot,
  findButton,
  findButtonIn,
  installConfigFetch,
  mountUnlocked,
  validationResponse,
} from './test/configurationWorkspace'

describe('ConfigurationWorkspace', () => {
  it('prevents invalid protocol, TLS, health, HTTP/2, and L4 pool combinations', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked()

    await wrapper.get('#mobile-object-navigation').setValue('listeners:2')
    const tcpService = wrapper.get('[data-field="listeners[].service"] select')
    expect(tcpService.attributes()).toHaveProperty('required')
    expect(tcpService.findAll('option').map((option) => option.text())).toEqual([
      'Select a service',
      'postgres',
    ])
    expect(wrapper.get('[data-field="listeners[].tls_profile"] select').attributes()).toHaveProperty('disabled')
    expect((wrapper.get('[data-field="listeners[].max_connections"] select').element as HTMLSelectElement).value).toBe('unbounded')

    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')
    const listenerTls = wrapper.get('[data-field="listeners[].tls_profile"] select')
    expect((listenerTls.element as HTMLSelectElement).value).toBe('public-sni')
    await wrapper.get('[data-field="listeners[].bind.type"] select').setValue('unix')
    const unixMode = wrapper.get('[data-field="listeners[].bind.mode"] input')
    await unixMode.setValue('0777')
    await unixMode.trigger('change')
    expect((wrapper.vm as unknown as { draft: CanonicalConfig }).draft.listeners[0]?.bind)
      .toEqual({ type: 'unix', path: '', mode: 0o777 })
    expect(listenerTls.attributes()).toHaveProperty('disabled')
    expect((listenerTls.element as HTMLSelectElement).selectedOptions[0]?.text).toBe('None')

    await wrapper.get('#mobile-object-navigation').setValue('upstream_pools:0')
    const tlsToggle = wrapper.get('[data-field="upstream_pools[].tls"] input')
    const h2Option = wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]')
    expect(tlsToggle.attributes()).toHaveProperty('disabled')
    expect(h2Option.attributes()).toHaveProperty('disabled')
    const endpointTypes = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.type"] select')
    await endpointTypes[1]!.setValue('unix')
    expect(wrapper.get('[data-field="upstream_pools[].health_check"] input').attributes()).toHaveProperty('disabled')
    expect(wrapper.get('[data-field="upstream_pools[].tls"] input').attributes()).toHaveProperty('disabled')
    expect((wrapper.get('[data-field="upstream_pools[].health_check"] input').element as HTMLInputElement).checked).toBe(false)
    await endpointTypes[1]!.setValue('dns')
    await tlsToggle.setValue(true)
    expect(wrapper.get('[data-field="upstream_pools[].health_check"] input').attributes()).toHaveProperty('disabled')
    expect(wrapper.get('[data-field="upstream_pools[].http_versions.max"] option[value="2"]').attributes('disabled')).toBeUndefined()

    await wrapper.get('#mobile-object-navigation').setValue('l4_services:0')
    const l4Pool = wrapper.get('[data-field="l4_services[].upstream_pool"] select')
    expect(l4Pool.findAll('option').map((option) => option.text())).not.toContain('secure-origins')
    expect((l4Pool.element as HTMLSelectElement).value).toBe('')
  })

  it('supports a complete mobile configuration edit and validation flow', async () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()

    const mobileAdds = wrapper.findAll('.mobile-add')
    expect(mobileAdds).toHaveLength(9)
    expect(mobileAdds.map((button) => button.attributes('aria-label'))).toEqual([
      'Add certificate',
      'Add SNI profile',
      'Add listener',
      'Add cache store',
      'Add upstream pool',
      'Add HTTP service',
      'Add forward proxy',
      'Add RTMP service',
      'Add L4 service',
    ])
    expect(mobileAdds.every((button) => button.attributes('type') === 'button')).toBe(true)
    await mobileAdds[0]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed certificate')
    await findButton(wrapper, 'Remove certificate').trigger('click')

    await mobileAdds[3]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed cache store')
    await findButton(wrapper, 'Remove cache store').trigger('click')

    await mobileAdds[6]!.trigger('click')
    expect(wrapper.get('.form-heading').text()).toContain('Unnamed forward proxy')
    await findButton(wrapper, 'Remove forward proxy').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('upstream_pools:0')
    await findButton(wrapper, 'Add server').trigger('click')
    const endpointTypes = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.type"] select')
    await endpointTypes.at(-1)!.setValue('unix')
    await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.path"] input').setValue('/run/oxiroute/mobile.sock')
    await wrapper.get('[aria-label="Remove endpoint 3"]').trigger('click')

    await wrapper.get('#mobile-object-navigation').setValue('rtmp_services:0')
    await wrapper.get('[data-field="rtmp_services[].applications[].name"] input').setValue('mobile-live')
    await findButton(wrapper, 'Add application').trigger('click')
    const names = wrapper.findAll('[data-field="rtmp_services[].applications[].name"] input')
    await names[1]!.setValue('mobile-backup')
    const recorderLists = wrapper.findAll('[data-field="rtmp_services[].applications[].recorders"]')
    await findButtonIn(recorderLists[1]!, 'Add recorder').trigger('click')
    await findButtonIn(recorderLists[1]!, 'Add recorder').trigger('click')
    const recorderNames = recorderLists[1]!.findAll('[data-field="rtmp_services[].applications[].recorders[].name"] input')
    await recorderNames[1]!.setValue('mobile-manual')
    await recorderLists[1]!.findAll('[data-field="rtmp_services[].applications[].recorders[].start"] select')[1]!.setValue('manual')
    await recorderLists[1]!.get('[aria-label="Remove recorder 1"]').trigger('click')
    await wrapper.get('[aria-label="Remove RTMP application 1"]').trigger('click')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.rtmp_services[0]?.applications).toEqual([
      expect.objectContaining({
        name: 'mobile-backup',
        live: true,
        idle_streams: true,
        recorders: [expect.objectContaining({ name: 'mobile-manual', start: 'manual' })],
      }),
    ])
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
  })

  it('edits fixed, redirect, and static actions through the authenticated mobile workspace', async () => {
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 390 })
    const fetch = installConfigFetch()
    const wrapper = await mountUnlocked()
    await wrapper.get('#mobile-object-navigation').setValue('http_services:0')
    const actionType = () => wrapper.get('[data-field="http_services[].routes[].action.type"] select')

    await actionType().setValue('fixed_response')
    await wrapper.get('[data-field="http_services[].routes[].action.status"] input').setValue(201)
    await wrapper.get('[data-field="http_services[].routes[].action.body"] textarea').setValue('created')

    await actionType().setValue('redirect')
    await wrapper.get('[data-field="http_services[].routes[].action.location.kind"] select').setValue('request_template')
    await wrapper.get('[data-field="http_services[].routes[].action.location.value"] input').setValue('https://$host$request_uri')

    await actionType().setValue('static_files')
    await wrapper.get('[data-field="http_services[].routes[].action.root_directory"] input').setValue('/srv/mobile-site')
    await wrapper.get('[data-field="http_services[].routes[].action.spa_fallback"] input').setValue('app.html')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const validationCall = fetch.mock.calls.find(([url]) => String(url) === '/api/v1/config/validate')
    const submitted = JSON.parse(String(validationCall?.[1]?.body)).config as CanonicalConfig
    expect(submitted.http_services[0]?.routes[0]?.action).toEqual({
      type: 'static_files',
      root_directory: '/srv/mobile-site',
      path_mapping: 'root',
      index_files: ['index.html'],
      internal_index_redirects: false,
      directory_redirects: false,
      spa_fallback: 'app.html',
      try_files: [],
      autoindex: false,
      autoindex_exact_size: true,
      autoindex_local_time: false,
      etag: true,
      mime: { default_type: null, types: [] },
      headers: [],
      error_responses: [],
    })
    expect(wrapper.get('.preview-panel').text()).toContain('KDL configuration preview')
  })

  it('guards browser unload and explicit draft reset', async () => {
    installConfigFetch()
    const confirm = vi.fn(() => false)
    vi.stubGlobal('confirm', confirm)
    const wrapper = await mountUnlocked()

    const version = wrapper.get('[data-field="version"] input')
    await version.setValue(2)
    const unload = new Event('beforeunload', { cancelable: true })
    expect(window.dispatchEvent(unload)).toBe(false)
    expect(unload.defaultPrevented).toBe(true)

    await findButton(wrapper, 'Reset draft').trigger('click')
    expect(confirm).toHaveBeenCalledOnce()
    expect((version.element as HTMLInputElement).value).toBe('2')

    confirm.mockReturnValue(true)
    await findButton(wrapper, 'Reset draft').trigger('click')
    expect((wrapper.get('[data-field="version"] input').element as HTMLInputElement).value).toBe('1')
  })

  it('keeps compositional roots read-only while preserving inspection and validation', async () => {
    const compositionalSnapshot = {
      ...configSnapshot(),
      configFormat: 'hocon' as const,
      compositional: true,
      dependencyCount: 2,
      configPreview: '{ version: 1 }',
    }
    const fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(compositionalSnapshot)
      if (url === '/api/v1/config/validate') {
        const body = JSON.parse(String(init?.body)) as { config: CanonicalConfig }
        return jsonResponse({
          ...validationResponse(body.config),
          configFormat: 'hocon',
          compositional: true,
          dependencyCount: 2,
          configPreview: '{ version: 1 }',
        })
      }
      throw new Error(`Unexpected request: ${url}`)
    })
    vi.stubGlobal('fetch', fetch)
    const wrapper = await mountUnlocked()

    expect(wrapper.get('.revision-banner.compositional').text()).toContain('Compositional root is read-only')
    expect(wrapper.get('.revision-banner.compositional').text()).toContain('2 dependencies')
    expect(wrapper.get('.editor-form').attributes()).toHaveProperty('inert')
    expect(wrapper.findAll('.mobile-add').every((button) => button.attributes('disabled') !== undefined)).toBe(true)
    expect(wrapper.findAll('.nav-add').every((button) => button.attributes('disabled') !== undefined)).toBe(true)

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.preview-panel').text()).toContain('HOCON configuration preview')
    expect(wrapper.get('.preview-panel pre').text()).toContain('{ version: 1 }')
    const reviewButton = findButton(wrapper, 'Review save')
    expect((reviewButton.element as HTMLButtonElement).disabled).toBe(true)
    expect(reviewButton.attributes('title')).toContain('cannot replace a compositional configuration root')
    expect(fetch.mock.calls.some(([, init]) => init?.method === 'PUT')).toBe(false)
  })
})
