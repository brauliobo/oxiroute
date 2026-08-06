import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { L4ServiceConfig, UpstreamPoolConfig } from '../config'
import NullableLimitField from './NullableLimitField.vue'
import UpstreamEndpointField from './UpstreamEndpointField.vue'
import UpstreamPoolEditor from './UpstreamPoolEditor.vue'

describe('configuration editor fields', () => {
  it('keeps invalid bounded limits unchanged and represents unbounded as null', async () => {
    const wrapper = mount(NullableLimitField, {
      props: {
        modelValue: 10_000,
        defaultValue: 10_000,
        fieldPath: 'listeners[].max_connections',
        legend: 'Concurrent connection limit',
        inputLabel: 'Maximum active connections',
      },
    })

    await wrapper.get('input').setValue(0)
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('10000')
    expect(wrapper.emitted('update:modelValue')).toBeUndefined()

    await wrapper.get('select').setValue('unbounded')
    expect(wrapper.emitted('update:modelValue')).toEqual([[null]])

    await wrapper.setProps({ modelValue: null })
    await wrapper.get('select').setValue('bounded')
    expect(wrapper.emitted('update:modelValue')?.at(-1)).toEqual([10_000])
  })

  it('replaces tagged endpoint variants instead of retaining fields from another type', async () => {
    const wrapper = mount(UpstreamEndpointField, {
      props: {
        endpoint: { type: 'socket', address: '127.0.0.1:3000' },
        index: 0,
      },
    })

    await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.type"] select').setValue('dns')

    expect(wrapper.emitted('update:endpoint')).toEqual([
      [{ type: 'dns', host: '', port: 80 }],
    ])
  })

  it('enforces endpoint and TLS restrictions inside the upstream editor', async () => {
    const pool: UpstreamPoolConfig = {
      name: 'origins',
      servers: [{ name: 'server-1', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' }],
      algorithm: 'round_robin',
      health_check: null,
      passive_health: null,
      tls: null,
      http_versions: { min: '1.1', max: '1.1' },
      queue_timeout_ms: null,
      connect_timeout_ms: null,
      server_timeout_ms: null,
      connection_reuse: 'safe',
    }
    const l4Services: L4ServiceConfig[] = [{
      name: 'database',
      upstream_pool: 'origins',
      connect_timeout_ms: 10_000,
      idle_timeout_ms: 300_000,
      lifetime_timeout_ms: null,
      udp: null,
    }]
    const wrapper = mount(UpstreamPoolEditor, { props: { pool, l4Services } })

    await wrapper.get('[data-field="upstream_pools[].tls"] input').setValue(true)
    expect(pool.tls).toEqual({ server_name: '', ca_certificate_path: null })
    expect(l4Services[0]?.upstream_pool).toBe('')

    await wrapper.get('[data-field="upstream_pools[].servers[].endpoint.type"] select').setValue('unix')
    expect(pool.servers[0]?.endpoint).toEqual({ type: 'unix', path: '' })
    expect(pool.tls).toBeNull()
    expect(pool.http_versions).toEqual({ min: '1.1', max: '1.1' })
    expect(wrapper.get('[data-field="upstream_pools[].health_check"] input').attributes()).toHaveProperty('disabled')
    expect(wrapper.get('[data-field="upstream_pools[].tls"] input').attributes()).toHaveProperty('disabled')
  })

  it('edits bounded weights only for weighted round-robin pools', async () => {
    const pool: UpstreamPoolConfig = {
      name: 'weighted-origins',
      servers: [
        { name: 'primary', endpoint: { type: 'socket', address: '127.0.0.1:3000' }, max_connections: null, dns_resolution: 'on_connect' },
        { name: 'secondary', endpoint: { type: 'socket', address: '127.0.0.1:3001' }, max_connections: null, dns_resolution: 'on_connect' },
      ],
      algorithm: { type: 'weighted_round_robin', weights: [3, 1] },
      health_check: null,
      passive_health: null,
      tls: null,
      http_versions: { min: '1.1', max: '1.1' },
      queue_timeout_ms: null,
      connect_timeout_ms: null,
      server_timeout_ms: null,
      connection_reuse: 'safe',
    }
    const wrapper = mount(UpstreamPoolEditor, { props: { pool, l4Services: [] } })

    const weights = wrapper.findAll('[data-field^="upstream_pools[].algorithm<weighted_round_robin>.weights"] input')
    expect(weights).toHaveLength(2)
    expect(weights[0]?.attributes()).toMatchObject({ min: '1', max: '100', step: '1' })

    await weights[0]!.setValue(101)
    expect(pool.algorithm).toEqual({ type: 'weighted_round_robin', weights: [3, 1] })
    expect(wrapper.get('.field-error').text()).toContain('1 to 100')

    await weights[0]!.setValue(25)
    expect(pool.algorithm).toEqual({ type: 'weighted_round_robin', weights: [25, 1] })

    await wrapper.get('.endpoint-editor .add-row').trigger('click')
    expect(pool.algorithm).toEqual({ type: 'weighted_round_robin', weights: [25, 1, 1] })
    await wrapper.get('.endpoint-editor .route-card .danger-link').trigger('click')
    expect(pool.algorithm).toEqual({ type: 'weighted_round_robin', weights: [1, 1] })

    await wrapper.get('[data-field="upstream_pools[].algorithm"] select').setValue('round_robin')
    expect(wrapper.findAll('[data-field^="upstream_pools[].algorithm<weighted_round_robin>.weights"]').length).toBe(0)
    expect(pool.algorithm).toBe('round_robin')
  })
})
