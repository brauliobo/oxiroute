<template lang="pug">
header.form-heading
  div
    p.eyebrow Origin group
    h3 {{ pool.name || 'Unnamed pool' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove pool
.field-grid
  label.field(data-field="upstream_pools[].name")
    span Stable name
    input(type="text" v-model="pool.name")
  label.field(data-field="upstream_pools[].algorithm")
    span Selection algorithm
    select(v-model="pool.algorithm")
      option(value="round_robin") Round robin
      option(value="least_connections") Least connections
fieldset.route-list.endpoint-editor(data-field="upstream_pools[].endpoints")
  .route-heading
    legend Endpoints
    button.add-row(type="button" @click="addEndpoint") + Add endpoint
  p.empty-list(v-if="pool.endpoints.length === 0") At least one endpoint is required.
  UpstreamEndpointField(
    v-for="(endpoint, endpointIndex) in pool.endpoints"
    :key="endpointIndex"
    :endpoint="endpoint"
    :index="endpointIndex"
    @update:endpoint="replaceEndpoint(endpointIndex, $event)"
    @remove="removeEndpoint(endpointIndex)"
  )
fieldset.object-block(data-field="upstream_pools[].health_check")
  legend Health check
  label.enable-row
    input(
      type="checkbox"
      :checked="pool.health_check !== null"
      :disabled="pool.tls !== null || hasUnixEndpoint"
      :title="healthCheckDisabledReason"
      @change="toggleHealthCheck"
    )
    span Enable active health checks
  template(v-if="pool.health_check")
    .field-grid
      label.field(data-field="upstream_pools[].health_check.type")
        span Probe type
        select(v-model="pool.health_check.type" @change="normalizeHealthFields")
          option(value="tcp") TCP connect
          option(value="http") HTTP 200
      label.field(data-field="upstream_pools[].health_check.interval_ms")
        span Interval (ms)
        input(type="number" min="1000" max="86400000" step="1" v-model.number="pool.health_check.interval_ms")
      label.field(data-field="upstream_pools[].health_check.timeout_ms")
        span Timeout (ms)
        input(type="number" min="1" max="30000" step="1" v-model.number="pool.health_check.timeout_ms")
      label.field(data-field="upstream_pools[].health_check.healthy_threshold")
        span Healthy threshold
        input(type="number" min="1" max="100" step="1" v-model.number="pool.health_check.healthy_threshold")
      label.field(data-field="upstream_pools[].health_check.unhealthy_threshold")
        span Unhealthy threshold
        input(type="number" min="1" max="100" step="1" v-model.number="pool.health_check.unhealthy_threshold")
    .field-grid(v-if="pool.health_check.type === 'http'")
      label.field(data-field="upstream_pools[].health_check.host")
        span Host authority
        input(type="text" :value="pool.health_check.host ?? ''" @input="pool.health_check.host = nullableInput($event)")
      label.field(data-field="upstream_pools[].health_check.path")
        span Probe path
        input(type="text" :value="pool.health_check.path ?? ''" @input="pool.health_check.path = nullableInput($event)")
fieldset.object-block(data-field="upstream_pools[].tls")
  legend Upstream TLS
  label.enable-row
    input(
      type="checkbox"
      :checked="pool.tls !== null"
      :disabled="pool.health_check !== null || hasUnixEndpoint"
      :title="tlsDisabledReason"
      @change="toggleTls"
    )
    span Enable verified origin TLS and SNI
  .field-grid(v-if="pool.tls")
    label.field(data-field="upstream_pools[].tls.server_name")
      span Verification server name
      input(type="text" v-model="pool.tls.server_name")
    label.field(data-field="upstream_pools[].tls.ca_certificate_path")
      span Custom CA certificate path
      input(type="text" :value="pool.tls.ca_certificate_path ?? ''" @input="pool.tls.ca_certificate_path = nullableInput($event)")
fieldset.object-block(data-field="upstream_pools[].http_versions")
  legend Upstream HTTP versions
  .field-grid
    label.field(data-field="upstream_pools[].http_versions.min")
      span Minimum
      select(v-model="pool.http_versions.min" @change="normalizeHttpVersions('min')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
    label.field(data-field="upstream_pools[].http_versions.max")
      span Maximum
      select(v-model="pool.http_versions.max" @change="normalizeHttpVersions('max')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
</template>

<script setup lang="ts">
import { computed } from 'vue'

import type { L4ServiceConfig, UpstreamEndpoint, UpstreamPoolConfig } from '../config'
import UpstreamEndpointField from './UpstreamEndpointField.vue'

const props = defineProps<{
  pool: UpstreamPoolConfig
  l4Services: L4ServiceConfig[]
}>()

const emit = defineEmits<{
  changed: []
  remove: []
}>()

const hasUnixEndpoint = computed(() => props.pool.endpoints.some(({ type }) => type === 'unix'))
const healthCheckDisabledReason = computed(() => {
  if (hasUnixEndpoint.value) return 'The server does not support health checks for Unix endpoints.'
  if (props.pool.tls !== null) return 'The server does not support active health checks with upstream TLS.'
  return undefined
})
const tlsDisabledReason = computed(() => {
  if (hasUnixEndpoint.value) return 'The server does not support upstream TLS for Unix endpoints.'
  if (props.pool.health_check !== null) return 'The server does not support upstream TLS with active health checks.'
  return undefined
})

function newEndpoint(): UpstreamEndpoint {
  return { type: 'socket', address: '127.0.0.1:3000' }
}

function addEndpoint(): void {
  props.pool.endpoints.push(newEndpoint())
  emit('changed')
}

function removeEndpoint(index: number): void {
  props.pool.endpoints.splice(index, 1)
  normalizeEndpointRestrictions()
  emit('changed')
}

function replaceEndpoint(index: number, endpoint: UpstreamEndpoint): void {
  props.pool.endpoints[index] = endpoint
  normalizeEndpointRestrictions()
}

function normalizeEndpointRestrictions(): void {
  if (!hasUnixEndpoint.value) return
  props.pool.health_check = null
  props.pool.tls = null
  props.pool.http_versions = { min: '1.1', max: '1.1' }
}

function toggleHealthCheck(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  if (enabled && hasUnixEndpoint.value) return
  if (enabled) {
    props.pool.tls = null
    props.pool.http_versions = { min: '1.1', max: '1.1' }
  }
  props.pool.health_check = enabled
    ? {
        type: 'tcp',
        interval_ms: 10_000,
        timeout_ms: 1_000,
        healthy_threshold: 1,
        unhealthy_threshold: 3,
        host: null,
        path: null,
      }
    : null
}

function normalizeHealthFields(): void {
  const health = props.pool.health_check
  if (!health) return
  if (health.type === 'http') {
    health.host ??= ''
    health.path ??= '/healthz'
  } else {
    health.host = null
    health.path = null
  }
}

function toggleTls(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  if (enabled && hasUnixEndpoint.value) return
  if (enabled) props.pool.health_check = null
  props.pool.tls = enabled ? { server_name: '', ca_certificate_path: null } : null
  if (!enabled) props.pool.http_versions = { min: '1.1', max: '1.1' }
  if (enabled) {
    for (const service of props.l4Services) {
      if (service.upstream_pool === props.pool.name) service.upstream_pool = ''
    }
  }
}

function normalizeHttpVersions(changed: 'min' | 'max'): void {
  const versions = props.pool.http_versions
  if (props.pool.tls === null) {
    versions.min = '1.1'
    versions.max = '1.1'
  } else if (changed === 'min' && versions.min === '2') {
    versions.max = '2'
  } else if (changed === 'max' && versions.max === '1.1') {
    versions.min = '1.1'
  }
}

function nullableInput(event: Event): string | null {
  return (event.target as HTMLInputElement).value || null
}
</script>
