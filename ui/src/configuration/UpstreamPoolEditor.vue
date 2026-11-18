<template lang="pug">
header.form-heading
  div
    p.eyebrow Origin group
    h3 {{ pool.name || 'Unnamed pool' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove pool
.field-grid
  ConfigField(path="upstream_pools[].name" label="Stable name")
    Input(v-model="pool.name")
  ConfigField(path="upstream_pools[].algorithm" label="Selection algorithm")
    Select(
      :model-value="algorithmType" @change="setAlgorithm"
      :options="[{ value: 'round_robin', label: 'Round robin' }, { value: 'weighted_round_robin', label: 'Weighted round robin' }, { value: 'least_connections', label: 'Least connections' }, { value: 'power_of_two_choices', label: 'Power of two choices' }, { value: 'first', label: 'First server' }]"
    )
ConfigGroup.route-list.endpoint-editor(path="upstream_pools[].servers")
  template(#legend)
    .route-heading
      legend Servers
      Button.add-row(@click="addServer") + Add server
  p.empty-list(v-if="pool.servers.length === 0") At least one server is required.
  ItemScope.route-card(v-for="(server, serverIndex) in pool.servers" :key="serverIndex" :index="serverIndex")
    header.route-card-heading
      strong Server {{ serverIndex + 1 }}
      Button.danger-link(variant="danger" small :aria-label="`Remove upstream server ${serverIndex + 1}`" @click="removeServer(serverIndex)") Remove
    ConfigField(path="upstream_pools[].servers[].name" label="Stable server name")
      Input(v-model="server.name" required)
    UpstreamEndpointField(
      :endpoint="server.endpoint"
      :index="serverIndex"
      @update:endpoint="replaceEndpoint(serverIndex, $event)"
      @remove="removeServer(serverIndex)"
    )
    label.field.weight-field(v-if="weightedRoundRobin")
      span Server weight
      NumberInput(:model-value="weightFor(serverIndex)" inputmode="numeric" :min="UPSTREAM_WEIGHT_MIN" :max="UPSTREAM_WEIGHT_MAX" step="1" required :aria-describedby="weightErrors[serverIndex] ? `server-weight-error-${serverIndex}` : undefined" :aria-invalid="weightErrors[serverIndex] ? 'true' : undefined" @input="clearWeightError(serverIndex, $event)" @change="setWeight(serverIndex, $event)")
      small Relative capacity for weighted round robin. Use an integer from 1 to 100.
      p.field-error(v-if="weightErrors[serverIndex]" :id="`server-weight-error-${serverIndex}`" role="alert") {{ weightErrors[serverIndex] }}
ConfigGroup.object-block(path="upstream_pools[].health_check" legend="Health check")
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
      ConfigField(path="upstream_pools[].health_check.type" label="Probe type")
        Select(
          v-model="pool.health_check.type" @change="normalizeHealthFields"
          :options="[{ value: 'tcp', label: 'TCP connect' }, { value: 'http', label: 'HTTP 200' }]"
        )
      ConfigField(path="upstream_pools[].health_check.interval_ms" label="Interval (ms)")
        NumberInput(min="1000" max="86400000" step="1" v-model="pool.health_check.interval_ms")
      ConfigField(path="upstream_pools[].health_check.timeout_ms" label="Timeout (ms)")
        NumberInput(min="1" max="30000" step="1" v-model="pool.health_check.timeout_ms")
      ConfigField(path="upstream_pools[].health_check.healthy_threshold" label="Healthy threshold")
        NumberInput(min="1" max="100" step="1" v-model="pool.health_check.healthy_threshold")
      ConfigField(path="upstream_pools[].health_check.unhealthy_threshold" label="Unhealthy threshold")
        NumberInput(min="1" max="100" step="1" v-model="pool.health_check.unhealthy_threshold")
    .field-grid(v-if="pool.health_check.type === 'http'")
      ConfigField(path="upstream_pools[].health_check.host" label="Host authority")
        Input(:model-value="pool.health_check.host ?? ''" @input="pool.health_check.host = nullableInput($event)")
      ConfigField(path="upstream_pools[].health_check.path" label="Probe path")
        Input(:model-value="pool.health_check.path ?? ''" @input="pool.health_check.path = nullableInput($event)")
ConfigGroup.object-block(path="upstream_pools[].passive_health" legend="Passive health")
  Checkbox.enable-row(:model-value="pool.passive_health !== null" @change="togglePassiveHealth") Enable passive health tracking
  template(v-if="pool.passive_health")
    .field-grid
      ConfigField(path="upstream_pools[].passive_health.observe" label="Observe")
        Select(
          v-model="pool.passive_health.observe"
          :options="[{ value: 'layer4', label: 'Layer 4' }, { value: 'layer7', label: 'Layer 7' }]"
        )
      ConfigField(path="upstream_pools[].passive_health.on_error" label="On error")
        Select(
          v-model="pool.passive_health.on_error"
          :options="[{ value: 'count', label: 'Count' }, { value: 'immediately', label: 'Immediately' }, { value: 'mark_down', label: 'Mark down' }]"
        )
      ConfigField(path="upstream_pools[].passive_health.error_limit" label="Error limit")
        NumberInput(min="1" max="100" step="1" v-model="pool.passive_health.error_limit")
      ConfigField(path="upstream_pools[].passive_health.mark_down" label="Mark down on threshold" check v-model="pool.passive_health.mark_down")
      ConfigField(path="upstream_pools[].passive_health.mark_up" label="Mark up after recovery" check v-model="pool.passive_health.mark_up")
      ConfigField(path="upstream_pools[].passive_health.initial_backoff_ms" label="Initial backoff (ms)")
        NumberInput(min="1" max="86400000" step="1" v-model="pool.passive_health.initial_backoff_ms")
      ConfigField(path="upstream_pools[].passive_health.max_backoff_ms" label="Maximum backoff (ms)")
        NumberInput(min="1" max="86400000" step="1" v-model="pool.passive_health.max_backoff_ms")
      ConfigField(path="upstream_pools[].passive_health.recovery_threshold" label="Recovery threshold")
        NumberInput(min="1" max="100" step="1" v-model="pool.passive_health.recovery_threshold")
ConfigGroup.object-block(path="upstream_pools[].tls" legend="Upstream TLS")
  label.enable-row
    input(
      type="checkbox"
      :checked="pool.tls !== null"
      :disabled="pool.health_check !== null || hasUnixEndpoint"
      :title="tlsDisabledReason"
      @change="toggleTls"
    )
    span Enable origin TLS
  .field-grid(v-if="pool.tls")
    ConfigField(path="upstream_pools[].tls.verify" label="Certificate verification")
      Select(
        :model-value="pool.tls.verify ?? 'full'" @change="setTlsVerify"
        :options="[{ value: 'full', label: 'Verify chain and server name' }, { value: 'none', label: 'None (accept any certificate)' }]"
      )
    ConfigField(path="upstream_pools[].tls.server_name" label="SNI and verification server name")
      Input(:model-value="pool.tls.server_name ?? ''" @input="pool.tls.server_name = nullableInput($event)")
    ConfigField(path="upstream_pools[].tls.ca_certificate_path" label="Custom CA certificate path")
      Input(:model-value="pool.tls.ca_certificate_path ?? ''" @input="pool.tls.ca_certificate_path = nullableInput($event)")
ConfigGroup.object-block(path="upstream_pools[].http_versions" legend="Upstream HTTP versions")
  .field-grid
    ConfigField(path="upstream_pools[].http_versions.min" label="Minimum")
      select.ox-input(v-model="pool.http_versions.min" @change="normalizeHttpVersions('min')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
    ConfigField(path="upstream_pools[].http_versions.max" label="Maximum")
      select.ox-input(v-model="pool.http_versions.max" @change="normalizeHttpVersions('max')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
ConfigGroup.object-block(path="upstream_pools[].resilience" legend="Resilience")
  ConfigField(path="upstream_pools[].resilience.fail_open" label="Serve the least-bad server when none is healthy" check v-model="pool.resilience.fail_open")
  .field-grid
    ConfigField(path="upstream_pools[].resilience.min_members" label="Minimum selectable servers kept by passive ejection")
      NumberInput(min="0" step="1" placeholder="1 with fail-open, otherwise 0" :model-value="pool.resilience.min_members ?? ''" @input="pool.resilience.min_members = nullableIntegerInput($event)")
    ConfigField(path="upstream_pools[].resilience.ejection_jitter_percent" label="Ejection duration jitter (%)")
      NumberInput(min="0" max="50" step="1" v-model="pool.resilience.ejection_jitter_percent")
    ConfigField(path="upstream_pools[].resilience.queue_while_unavailable_ms" label="Wait for a recovering server (ms)")
      NumberInput(min="1" max="86400000" step="1" :model-value="pool.resilience.queue_while_unavailable_ms ?? ''" @input="pool.resilience.queue_while_unavailable_ms = nullableIntegerInput($event)")
    ConfigField(path="upstream_pools[].resilience.unavailable_retry_after_seconds" label="Retry-After of the unavailable 503 (s)")
      NumberInput(min="1" max="86400" step="1" :model-value="pool.resilience.unavailable_retry_after_seconds ?? ''" @input="pool.resilience.unavailable_retry_after_seconds = nullableIntegerInput($event)")
    ConfigField(path="upstream_pools[].resilience.unavailable_body" label="Body of the unavailable 503")
      Input(maxlength="1024" :model-value="pool.resilience.unavailable_body ?? ''" @input="pool.resilience.unavailable_body = nullableInput($event)")
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
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
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

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
const algorithmType = computed(() => isWeightedRoundRobin(props.pool.algorithm)
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
  } else if (
    value === 'round_robin' || value === 'least_connections' ||
    value === 'power_of_two_choices' || value === 'first'
  ) {
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
  props.pool.tls = enabled ? { server_name: '', verify: 'full', ca_certificate_path: null } : null
  if (!enabled) props.pool.http_versions = { min: '1.1', max: '1.1' }
  if (enabled) {
    for (const service of props.l4Services) {
      if (service.upstream_pool === props.pool.name) service.upstream_pool = ''
    }
  }
}

function setTlsVerify(event: Event): void {
  const tls = props.pool.tls
  if (!tls) return
  tls.verify = (event.target as HTMLSelectElement).value === 'none' ? 'none' : 'full'
  if (tls.verify === 'none') tls.ca_certificate_path = null
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

function nullableIntegerInput(event: Event): number | null {
  const value = (event.target as HTMLInputElement).value
  return value === '' ? null : Number(value)
}

function isWeightedRoundRobin(algorithm: UpstreamAlgorithm): algorithm is WeightedRoundRobinAlgorithm {
  return typeof algorithm === 'object' && algorithm.type === 'weighted_round_robin'
}
</script>
