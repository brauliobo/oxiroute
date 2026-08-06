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
    select(:value="algorithmType" @change="setAlgorithm")
      option(value="round_robin") Round robin
      option(value="weighted_round_robin") Weighted round robin
      option(value="least_connections") Least connections
      option(value="first") First server
fieldset.route-list.endpoint-editor(data-field="upstream_pools[].servers")
  .route-heading
    legend Servers
    button.add-row(type="button" @click="addServer") + Add server
  p.empty-list(v-if="pool.servers.length === 0") At least one server is required.
  article.route-card(v-for="(server, serverIndex) in pool.servers" :key="serverIndex")
    header.route-card-heading
      strong Server {{ serverIndex + 1 }}
      button.danger-link(type="button" :aria-label="`Remove upstream server ${serverIndex + 1}`" @click="removeServer(serverIndex)") Remove
    label.field(data-field="upstream_pools[].servers[].name")
      span Stable server name
      input(type="text" v-model="server.name" required)
    UpstreamEndpointField(
      :endpoint="server.endpoint"
      :index="serverIndex"
      @update:endpoint="replaceEndpoint(serverIndex, $event)"
      @remove="removeServer(serverIndex)"
    )
    label.field.weight-field(
      v-if="weightedRoundRobin"
      data-field="upstream_pools[].algorithm<weighted_round_robin>.weights[]"
    )
      span Server weight
      input(
        :value="weightFor(serverIndex)"
        type="number"
        inputmode="numeric"
        :min="UPSTREAM_WEIGHT_MIN"
        :max="UPSTREAM_WEIGHT_MAX"
        step="1"
        required
        :aria-describedby="weightErrors[serverIndex] ? `server-weight-error-${serverIndex}` : undefined"
        :aria-invalid="weightErrors[serverIndex] ? 'true' : undefined"
        @input="clearWeightError(serverIndex, $event)"
        @change="setWeight(serverIndex, $event)"
      )
      small Relative capacity for weighted round robin. Use an integer from 1 to 100.
      p.field-error(v-if="weightErrors[serverIndex]" :id="`server-weight-error-${serverIndex}`" role="alert") {{ weightErrors[serverIndex] }}
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
fieldset.object-block(data-field="upstream_pools[].passive_health")
  legend Passive health
  label.enable-row
    input(type="checkbox" :checked="pool.passive_health !== null" @change="togglePassiveHealth")
    span Enable passive health tracking
  template(v-if="pool.passive_health")
    .field-grid
      label.field(data-field="upstream_pools[].passive_health.observe")
        span Observe
        select(v-model="pool.passive_health.observe")
          option(value="layer4") Layer 4
          option(value="layer7") Layer 7
      label.field(data-field="upstream_pools[].passive_health.on_error")
        span On error
        select(v-model="pool.passive_health.on_error")
          option(value="count") Count
          option(value="immediately") Immediately
          option(value="mark_down") Mark down
      label.field(data-field="upstream_pools[].passive_health.error_limit")
        span Error limit
        input(type="number" min="1" max="100" step="1" v-model.number="pool.passive_health.error_limit")
      label.enable-row(data-field="upstream_pools[].passive_health.mark_down")
        input(type="checkbox" v-model="pool.passive_health.mark_down")
        span Mark down on threshold
      label.enable-row(data-field="upstream_pools[].passive_health.mark_up")
        input(type="checkbox" v-model="pool.passive_health.mark_up")
        span Mark up after recovery
      label.field(data-field="upstream_pools[].passive_health.initial_backoff_ms")
        span Initial backoff (ms)
        input(type="number" min="1" max="86400000" step="1" v-model.number="pool.passive_health.initial_backoff_ms")
      label.field(data-field="upstream_pools[].passive_health.max_backoff_ms")
        span Maximum backoff (ms)
        input(type="number" min="1" max="86400000" step="1" v-model.number="pool.passive_health.max_backoff_ms")
      label.field(data-field="upstream_pools[].passive_health.recovery_threshold")
        span Recovery threshold
        input(type="number" min="1" max="100" step="1" v-model.number="pool.passive_health.recovery_threshold")
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
import { computed, ref } from 'vue'

import {
  UPSTREAM_WEIGHT_MAX,
  UPSTREAM_WEIGHT_MIN,
  type L4ServiceConfig,
  type UpstreamAlgorithm,
  type UpstreamEndpoint,
  type UpstreamPoolConfig,
  type UpstreamServerConfig,
  type WeightedRoundRobinAlgorithm,
} from '../config'
import UpstreamEndpointField from './UpstreamEndpointField.vue'

const props = defineProps<{
  pool: UpstreamPoolConfig
  l4Services: L4ServiceConfig[]
}>()

const emit = defineEmits<{
  changed: []
  remove: []
}>()

const weightErrors = ref<Record<number, string>>({})
const hasUnixEndpoint = computed(() => props.pool.servers.some(({ endpoint }) => endpoint.type === 'unix'))
const weightedRoundRobin = computed(() => isWeightedRoundRobin(props.pool.algorithm))
const algorithmType = computed(() => weightedRoundRobin.value
  ? 'weighted_round_robin'
  : props.pool.algorithm)
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

function newServer(): UpstreamServerConfig {
  return {
    name: `server-${props.pool.servers.length + 1}`,
    endpoint: { type: 'socket', address: '127.0.0.1:3000' },
    max_connections: null,
    dns_resolution: 'on_connect',
  }
}

function addServer(): void {
  props.pool.servers.push(newServer())
  if (isWeightedRoundRobin(props.pool.algorithm)) props.pool.algorithm.weights.push(UPSTREAM_WEIGHT_MIN)
  emit('changed')
}

function removeServer(index: number): void {
  props.pool.servers.splice(index, 1)
  if (isWeightedRoundRobin(props.pool.algorithm)) props.pool.algorithm.weights.splice(index, 1)
  weightErrors.value = {}
  normalizeEndpointRestrictions()
  emit('changed')
}

function setAlgorithm(event: Event): void {
  const value = (event.target as HTMLSelectElement).value
  if (value === 'weighted_round_robin') {
    props.pool.algorithm = {
      type: 'weighted_round_robin',
      weights: props.pool.servers.map(() => UPSTREAM_WEIGHT_MIN),
    }
  } else if (value === 'round_robin' || value === 'least_connections' || value === 'first') {
    props.pool.algorithm = value
  }
  weightErrors.value = {}
  emit('changed')
}

function weightFor(index: number): number {
  return isWeightedRoundRobin(props.pool.algorithm)
    ? props.pool.algorithm.weights[index] ?? UPSTREAM_WEIGHT_MIN
    : UPSTREAM_WEIGHT_MIN
}

function clearWeightError(index: number, event: Event): void {
  const input = event.target as HTMLInputElement
  delete weightErrors.value[index]
  input.setCustomValidity('')
}

function setWeight(index: number, event: Event): void {
  const input = event.target as HTMLInputElement
  const value = Number(input.value)
  if (!Number.isInteger(value) || value < UPSTREAM_WEIGHT_MIN || value > UPSTREAM_WEIGHT_MAX) {
    const message = `Weight must be an integer from ${UPSTREAM_WEIGHT_MIN} to ${UPSTREAM_WEIGHT_MAX}.`
    weightErrors.value[index] = message
    input.setCustomValidity(message)
    return
  }
  if (!weightedRoundRobin.value) return
  delete weightErrors.value[index]
  input.setCustomValidity('')
  if (!isWeightedRoundRobin(props.pool.algorithm)) return
  props.pool.algorithm.weights[index] = value
  emit('changed')
}

function replaceEndpoint(index: number, endpoint: UpstreamEndpoint): void {
  const server = props.pool.servers[index]
  if (!server) return
  server.endpoint = endpoint
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
        startup: 'checking',
        fast_interval_ms: null,
        down_interval_ms: null,
        host: null,
        path: null,
        expected_status: null,
        http_version: null,
      }
    : null
}

function togglePassiveHealth(event: Event): void {
  const enabled = (event.target as HTMLInputElement).checked
  props.pool.passive_health = enabled
    ? {
        observe: 'layer7',
        on_error: 'count',
        error_limit: 3,
        mark_down: false,
        mark_up: false,
        initial_backoff_ms: 30_000,
        max_backoff_ms: 300_000,
        recovery_threshold: 1,
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

function isWeightedRoundRobin(algorithm: UpstreamAlgorithm): algorithm is WeightedRoundRobinAlgorithm {
  return typeof algorithm === 'object' && algorithm.type === 'weighted_round_robin'
}
</script>

<style scoped>
.weight-field {
  margin-top: 16px;
}

.field-error {
  margin: 0;
  color: #ff9b88;
  font-size: 0.7rem;
  font-weight: 400;
  letter-spacing: 0;
  line-height: 1.45;
  text-transform: none;
}

.weight-field input:invalid {
  border-color: #ff745c;
}
</style>
