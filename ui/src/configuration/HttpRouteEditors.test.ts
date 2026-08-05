import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { HttpServiceConfig } from '../config'
import HttpServiceEditor from './HttpServiceEditor.vue'
import { defaultHttpRoute } from './httpDefaults'

function service(): HttpServiceConfig {
  return {
    name: 'web',
    routes: [defaultHttpRoute()],
    automatic_response_headers: true,
    upstream_io_timeout_ms: 30_000,
    max_request_body_bytes: 10_485_760,
    gzip: null,
    access_log: null,
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
        upstream_pool: 'origins',
        policy: {
          upstream_host: { type: 'endpoint', unix_fallback: 'localhost' },
          upstream_path_rewrite: null,
          request_headers: [{ operation: 'set', name: 'x-client-ip', value: { type: 'client_ip' } }],
          response_headers: [{ operation: 'set', name: 'cache-control', value: 'private', always: true }],
          response_cookie_path_rewrites: [{ from: '/internal', to: '/public' }],
          response_cookie_attributes: [],
          retry: {
            max_retries: 2,
            target: 'next_server',
            delay_ms: 0,
            final_redispatch: false,
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

  it('edits ASCII case-insensitive authority matching and gates final redispatch', async () => {
    const model = service()
    const wrapper = mount(HttpServiceEditor, {
      props: { service: model, poolNames: ['origins'], cacheStoreNames: [] },
    })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)

    await field('http_services[].routes[].host').get('input').setValue(true)
    const hostKind = field('http_services[].routes[].host.kind').get('select')
    await hostKind.setValue('ascii_case_insensitive_exact_authority')
    const hostValue = field('http_services[].routes[].host.value')
    expect(hostValue.text()).toContain('Authority value')
    expect(hostValue.get('input').attributes('placeholder')).toBe('API.Example.Test:8443')
    await hostValue.get('input').setValue('API.Example.Test:8443')

    const maxRetries = field('http_services[].routes[].action.policy.retry.max_retries').get('input')
    const target = field('http_services[].routes[].action.policy.retry.target').get('select')
    const finalRedispatch = field('http_services[].routes[].action.policy.retry.final_redispatch').get('input')
    expect(maxRetries.attributes('max')).toBe('3')
    expect(finalRedispatch.attributes()).toHaveProperty('disabled')
    await target.setValue('same_server')
    await maxRetries.setValue(3)
    expect(finalRedispatch.attributes()).not.toHaveProperty('disabled')
    await finalRedispatch.setValue(true)
    expect(model.routes[0]?.action).toMatchObject({
      policy: {
        retry: { max_retries: 3, target: 'same_server', final_redispatch: true },
      },
    })

    await maxRetries.setValue('')
    expect(model.routes[0]?.action).toMatchObject({
      policy: { retry: { max_retries: 3, final_redispatch: true } },
    })
    await maxRetries.setValue(0)
    expect(model.routes[0]?.action).toMatchObject({
      policy: { retry: { max_retries: 0, final_redispatch: false } },
    })
    await maxRetries.setValue(3)
    await finalRedispatch.setValue(true)

    await target.setValue('next_server')
    expect(finalRedispatch.attributes()).toHaveProperty('disabled')
    expect(model.routes[0]?.action).toMatchObject({
      policy: { retry: { target: 'next_server', final_redispatch: false } },
    })
  })

  it('edits X-Forwarded-For source CIDR exceptions', async () => {
    const model = service()
    const wrapper = mount(HttpServiceEditor, {
      props: { service: model, poolNames: ['origins'], cacheStoreNames: [] },
    })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)

    await button(wrapper, 'Add request mutation').trigger('click')
    await field('http_services[].routes[].action.policy.request_headers[].operation').get('select').setValue('set')
    await field('http_services[].routes[].action.policy.request_headers[].name').get('input').setValue('x-forwarded-for')
    await field('http_services[].routes[].action.policy.request_headers[].value.type').get('select')
      .setValue('appended_x_forwarded_for')
    await button(wrapper, 'Add source CIDR').trigger('click')
    await field('http_services[].routes[].action.policy.request_headers[].value.except_source_cidrs')
      .get('input').setValue('127.0.0.0/8')

    expect(model.routes[0]?.action).toMatchObject({
      type: 'proxy',
      policy: {
        request_headers: [{
          operation: 'set',
          name: 'x-forwarded-for',
          value: {
            type: 'appended_x_forwarded_for',
            max_bytes: 8_192,
            except_source_cidrs: ['127.0.0.0/8'],
          },
        }],
      },
    })
  })

  it('edits nginx Host fallbacks and response-header status scope', async () => {
    const model = service()
    const wrapper = mount(HttpServiceEditor, {
      props: { service: model, poolNames: ['origins'], cacheStoreNames: [] },
    })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)

    await field('http_services[].routes[].action.policy.upstream_host.type').get('select')
      .setValue('nginx_host')
    await field('http_services[].routes[].action.policy.upstream_host.fallback').get('input')
      .setValue('default.example')
    await button(wrapper, 'Add request mutation').trigger('click')
    await field('http_services[].routes[].action.policy.request_headers[].operation').get('select')
      .setValue('set')
    await field('http_services[].routes[].action.policy.request_headers[].value.type').get('select')
      .setValue('nginx_host')
    await field('http_services[].routes[].action.policy.request_headers[].value.fallback').get('input')
      .setValue('header.example')
    expect(model.routes[0]?.action).toMatchObject({
      policy: {
        upstream_host: { type: 'nginx_host', fallback: 'default.example' },
        request_headers: [{ value: { type: 'nginx_host', fallback: 'header.example' } }],
      },
    })

    await field('http_services[].routes[].action.type').get('select').setValue('fixed_response')
    await button(wrapper, 'Add header').trigger('click')
    await field('http_services[].routes[].action.headers[].always').get('input').setValue(true)
    expect(model.routes[0]?.action).toMatchObject({ headers: [{ always: true }] })

    await field('http_services[].routes[].action.type').get('select').setValue('redirect')
    await field('http_services[].routes[].action.location.kind').get('select')
      .setValue('request_template')
    await field('http_services[].routes[].action.location.nginx_host_fallback').get('input')
      .setValue('redirect.example')
    expect(model.routes[0]?.action).toMatchObject({
      location: { nginx_host_fallback: 'redirect.example' },
    })
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
      headers: [{ name: 'x-fixed', value: 'yes', always: false }],
    })
    expect(field('http_services[].routes[].action.body').get('textarea').attributes('aria-describedby')).toBeTruthy()

    await actionType().setValue('redirect')
    await field('http_services[].routes[].action.status').get('select').setValue('308')
    await field('http_services[].routes[].action.location.kind').get('select').setValue('request_template')
    await field('http_services[].routes[].action.location.value').get('input').setValue('https://$host$request_uri')
    expect(model.routes[0]?.action).toEqual({
      type: 'redirect',
      status: 308,
      location: {
        kind: 'request_template',
        value: 'https://$host$request_uri',
        nginx_host_fallback: null,
      },
      headers: [],
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
      path_mapping: 'root',
      index_files: ['index.html', 'home.html'],
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
