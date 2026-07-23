import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { HttpServiceConfig } from '../config'
import HttpServiceEditor from './HttpServiceEditor.vue'
import { defaultHttpRoute } from './httpDefaults'

function service(): HttpServiceConfig {
  return {
    name: 'web',
    routes: [defaultHttpRoute()],
    upstream_io_timeout_ms: 30_000,
    max_request_body_bytes: 10_485_760,
  }
}

describe('canonical HTTP route editors', () => {
  it('edits matchers, bearer access, proxy Host policy, mutations, cookies, and retry safety', async () => {
    const model = service()
    const wrapper = mount(HttpServiceEditor, {
      props: { service: model, poolNames: ['origins'], cacheStoreNames: [] },
    })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)

    await field('http_services[].routes[].host').get('input').setValue(true)
    await field('http_services[].routes[].host.kind').get('select').setValue('exact_authority')
    await field('http_services[].routes[].host.value').get('input').setValue('api.example.test:8443')
    await field('http_services[].routes[].path.kind').get('select').setValue('exact')
    await field('http_services[].routes[].path.value').get('input').setValue('/v2')

    const methods = field('http_services[].routes[].methods')
    await methods.get('.add-row').trigger('click')
    await methods.get('input').setValue('POST')

    await field('http_services[].routes[].access_policy').get('input').setValue(true)
    await field('http_services[].routes[].access_policy.token_file_path').get('input').setValue('/run/route.token')
    await field('http_services[].routes[].access_policy.header_name').get('input').setValue('x-route-token')
    await field('http_services[].routes[].access_policy.realm').get('input').setValue('private-api')

    await field('http_services[].routes[].action.upstream_pool').get('select').setValue('origins')
    await field('http_services[].routes[].action.policy.upstream_host.type').get('select').setValue('endpoint')
    await field('http_services[].routes[].action.policy.upstream_host.unix_fallback').get('input').setValue('localhost')

    await button(wrapper, 'Add request mutation').trigger('click')
    await field('http_services[].routes[].action.policy.request_headers[].operation').get('select').setValue('set')
    await field('http_services[].routes[].action.policy.request_headers[].name').get('input').setValue('x-client-ip')
    await field('http_services[].routes[].action.policy.request_headers[].value.type').get('select').setValue('client_ip')

    await button(wrapper, 'Add response mutation').trigger('click')
    await field('http_services[].routes[].action.policy.response_headers[].operation').get('select').setValue('set')
    await field('http_services[].routes[].action.policy.response_headers[].name').get('input').setValue('cache-control')
    await field('http_services[].routes[].action.policy.response_headers[].value').get('input').setValue('private')

    await button(wrapper, 'Add cookie rewrite').trigger('click')
    await field('http_services[].routes[].action.policy.response_cookie_path_rewrites[].from').get('input').setValue('/internal')
    await field('http_services[].routes[].action.policy.response_cookie_path_rewrites[].to').get('input').setValue('/public')
    await field('http_services[].routes[].action.policy.retry.max_retries').get('input').setValue(2)

    expect(model.routes[0]).toEqual({
      host: { kind: 'exact_authority', value: 'api.example.test:8443' },
      path: { kind: 'exact', value: '/v2' },
      methods: ['POST'],
      access_policy: {
        type: 'bearer_token_file',
        token_file_path: '/run/route.token',
        header_name: 'x-route-token',
        realm: 'private-api',
      },
      action: {
        type: 'proxy',
        upstream_pool: 'origins',
        policy: {
          upstream_host: { type: 'endpoint', unix_fallback: 'localhost' },
          request_headers: [{ operation: 'set', name: 'x-client-ip', value: { type: 'client_ip' } }],
          response_headers: [{ operation: 'set', name: 'cache-control', value: 'private' }],
          response_cookie_path_rewrites: [{ from: '/internal', to: '/public' }],
          retry: {
            max_retries: 2,
            triggers: ['connect_failure', 'connect_timeout', 'refused_stream'],
            method_safety: 'get_head',
            body_safety: 'empty',
          },
          cache: null,
        },
      },
    })
    expect(field('http_services[].routes[].action.policy.retry.method_safety').get('select').attributes('title'))
      .toContain('GET and HEAD')
    expect(field('http_services[].routes[].action.policy.retry.body_safety').get('select').attributes('title'))
      .toContain('empty request body')
  })

  it('replaces tagged actions exactly and exposes accessible fixed, redirect, and static controls', async () => {
    const model = service()
    const wrapper = mount(HttpServiceEditor, {
      props: { service: model, poolNames: ['origins'], cacheStoreNames: [] },
    })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)
    const actionType = () => field('http_services[].routes[].action.type').get('select')

    await actionType().setValue('fixed_response')
    await field('http_services[].routes[].action.status').get('input').setValue(204)
    await field('http_services[].routes[].action.body').get('textarea').setValue('not allowed')
    expect(field('http_services[].routes[].action.body').get('textarea').attributes('aria-invalid')).toBe('true')
    expect(field('http_services[].routes[].action.body').get('[role="alert"]').text()).toContain('status 204')
    await field('http_services[].routes[].action.body').get('textarea').setValue('')
    await button(wrapper, 'Add header').trigger('click')
    await field('http_services[].routes[].action.headers[].name').get('input').setValue('x-fixed')
    await field('http_services[].routes[].action.headers[].value').get('input').setValue('yes')
    expect(model.routes[0]?.action).toEqual({
      type: 'fixed_response',
      status: 204,
      body: '',
      headers: [{ name: 'x-fixed', value: 'yes' }],
    })
    expect(field('http_services[].routes[].action.body').get('textarea').attributes('aria-describedby')).toBeTruthy()

    await actionType().setValue('redirect')
    await field('http_services[].routes[].action.status').get('select').setValue('308')
    await field('http_services[].routes[].action.location.kind').get('select').setValue('request_template')
    await field('http_services[].routes[].action.location.value').get('input').setValue('https://$host$request_uri')
    expect(model.routes[0]?.action).toEqual({
      type: 'redirect',
      status: 308,
      location: { kind: 'request_template', value: 'https://$host$request_uri' },
    })
    expect(field('http_services[].routes[].action.location.value').text()).toContain('$scheme')

    await actionType().setValue('static_files')
    await field('http_services[].routes[].action.root_directory').get('input').setValue('/srv/site')
    await field('http_services[].routes[].action.spa_fallback').get('input').setValue('app.html')
    const indexes = field('http_services[].routes[].action.index_files')
    await indexes.get('.add-row').trigger('click')
    await indexes.findAll('input')[1]!.setValue('home.html')
    expect(model.routes[0]?.action).toEqual({
      type: 'static_files',
      root_directory: '/srv/site',
      index_files: ['index.html', 'home.html'],
      spa_fallback: 'app.html',
    })
    expect(field('http_services[].routes[].action.root_directory').text()).toContain('Authenticated configuration only')

    for (const control of wrapper.findAll('input, select, textarea')) {
      const element = control.element as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement
      const explicitlyLabeled = Boolean(element.id && wrapper.find(`label[for="${element.id}"]`).exists())
      expect(element.closest('label') !== null || explicitlyLabeled).toBe(true)
    }
  })
})

function button(wrapper: ReturnType<typeof mount>, text: string) {
  const match = wrapper.findAll('button').find((candidate) => candidate.text().includes(text))
  if (!match) throw new Error(`Button not found: ${text}`)
  return match
}
