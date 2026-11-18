import { flushPromises } from './test/mount'
import { describe, expect, it, vi } from 'vitest'

import type { ConfigDiagnostic } from './config'
import { jsonResponse } from './test/contractFixtures'
import {
  configSnapshot,
  findButton,
  installConfigFetch,
  mountUnlocked,
  validationResponse,
} from './test/configurationWorkspace'

const bindDiagnostic: ConfigDiagnostic = {
  code: 'E_INVALID_LISTENER_TRANSPORT',
  severity: 'error',
  stage: 'validation',
  path: '/listeners/https/bind',
  message: 'The listener bind is invalid.',
  detail: 'ipv6_only is only valid on a `[::]` socket bind',
}

const serverDiagnostic: ConfigDiagnostic = {
  code: 'E_INVALID_UPSTREAM_SERVER',
  severity: 'error',
  stage: 'validation',
  path: '/upstream_pools/web-origins/servers/server-2/dns_resolution',
  message: 'The upstream server is invalid.',
  detail: 'startup resolution applies only to DNS endpoints',
}

const unplacedDiagnostic: ConfigDiagnostic = {
  code: 'E_INVALID_FIELD',
  severity: 'error',
  stage: 'parse',
  path: '/config',
  message: 'the config field does not match the canonical configuration schema',
  detail: 'invalid value, expected socket address',
}

function rejectValidation(diagnostics: ConfigDiagnostic[]) {
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    if (url === '/api/v1/config' && !init?.method) return jsonResponse(configSnapshot())
    if (url === '/api/v1/config/validate') return jsonResponse({ diagnostics }, 422)
    throw new Error(`Unexpected request: ${url}`)
  }))
}

describe('ConfigurationWorkspace validation diagnostics', () => {
  it('shows the message, detail and path beside the offending field and wires it to the control', async () => {
    rejectValidation([bindDiagnostic])
    const wrapper = await mountUnlocked(true)
    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')

    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const group = wrapper.get('[data-field="listeners[].bind"]')
    const message = group.get('.field-error')
    expect(message.text()).toContain('The listener bind is invalid.')
    expect(message.text()).toContain('ipv6_only is only valid')
    expect(message.text()).toContain('/listeners/https/bind')
    expect(group.attributes('aria-invalid')).toBe('true')
    expect(group.attributes('aria-describedby')).toContain(message.attributes('id'))
    expect(wrapper.find('[data-field="listeners[].name"] .field-error').exists()).toBe(false)
  })

  it('places diagnostics named by item name or index on the matching nested item', async () => {
    rejectValidation([
      { ...serverDiagnostic, path: '/upstream_pools/web-origins/servers/server-2/endpoint.host', message: 'Host by name.' },
      { ...serverDiagnostic, path: '/upstream_pools/web-origins/servers/0/endpoint.address', message: 'Address by index.' },
    ])
    const wrapper = await mountUnlocked(true)
    await wrapper.get('#mobile-object-navigation').setValue('upstream_pools:0')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const addresses = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.address"]')
    const hosts = wrapper.findAll('[data-field="upstream_pools[].servers[].endpoint.host"]')
    expect(wrapper.findAll('.field-error')).toHaveLength(2)
    expect(addresses).toHaveLength(1)
    expect(addresses[0]!.get('.field-error').text()).toContain('Address by index.')
    expect(hosts).toHaveLength(1)
    expect(hosts[0]!.get('.field-error').text()).toContain('Host by name.')
  })

  it('marks the control invalid and describes it with the message', async () => {
    rejectValidation([{ ...bindDiagnostic, path: '/listeners/https/protocol', message: 'The protocol is invalid.' }])
    const wrapper = await mountUnlocked(true)
    await wrapper.get('#mobile-object-navigation').setValue('listeners:0')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const control = wrapper.get('[data-field="listeners[].protocol"] select')
    const message = wrapper.get('[data-field="listeners[].protocol"] .field-error')
    expect(control.attributes('aria-invalid')).toBe('true')
    expect(control.attributes('aria-describedby')).toBe(message.attributes('id'))
  })

  it('does not show a field message while another object is selected', async () => {
    rejectValidation([bindDiagnostic])
    const wrapper = await mountUnlocked(true)
    await wrapper.get('#mobile-object-navigation').setValue('listeners:1')
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.find('.field-error').exists()).toBe(false)
    expect(wrapper.get('.diagnostic-list').text()).toContain('The listener bind is invalid.')
  })

  it('keeps unplaceable diagnostics in the list with their detail and no jump button', async () => {
    rejectValidation([unplacedDiagnostic])
    const wrapper = await mountUnlocked(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const list = wrapper.get('.diagnostic-list')
    expect(list.text()).toContain('invalid value, expected socket address')
    expect(list.text()).toContain('/config')
    expect(list.find('button').exists()).toBe(false)
    expect(wrapper.find('.field-error').exists()).toBe(false)
  })

  it('summarizes counts next to Validate and moves focus to the diagnostics after Validate', async () => {
    rejectValidation([bindDiagnostic, serverDiagnostic, { ...unplacedDiagnostic, severity: 'warning' }])
    const wrapper = await mountUnlocked(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    expect(wrapper.get('.diagnostic-summary').text()).toBe('2 errors, 1 warning')
    expect(document.activeElement).toBe(wrapper.get('#diagnostics-heading').element)
  })

  it('announces the validation outcome to assistive technology', async () => {
    rejectValidation([bindDiagnostic])
    const wrapper = await mountUnlocked(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()

    const alert = document.querySelector('[role="alert"].ox-sr-only')
    expect(alert?.textContent).toBe('Validation finished: 1 error.')
  })

  it('announces a clean validation without moving focus', async () => {
    installConfigFetch()
    const wrapper = await mountUnlocked(true)
    const validate = findButton(wrapper, 'Validate candidate')
    ;(validate.element as HTMLElement).focus()
    await validate.trigger('click')
    await flushPromises()

    expect(wrapper.get('.diagnostic-summary').text()).toBe('Validated: no diagnostics')
    expect(document.activeElement).toBe(validate.element)
    expect(document.querySelector('[role="status"].ox-sr-only')?.textContent).toBe('Validation finished: No diagnostics.')
  })

  it('jumps from a listed diagnostic to the field that shows it', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/config' && !init?.method) return jsonResponse(configSnapshot())
      const body = JSON.parse(String(init?.body)) as { config: Parameters<typeof validationResponse>[0] }
      return jsonResponse(validationResponse(body.config, [bindDiagnostic]))
    }))
    const wrapper = await mountUnlocked(true)
    await findButton(wrapper, 'Validate candidate').trigger('click')
    await flushPromises()
    await findButton(wrapper, bindDiagnostic.message).trigger('click')
    await flushPromises()

    expect(document.activeElement).toBe(wrapper.get('[data-field="listeners[].bind"]').element)
  })
})
