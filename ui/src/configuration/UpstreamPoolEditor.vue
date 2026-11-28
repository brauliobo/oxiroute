<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow Origin group
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
    .cfg-route-heading
      legend Servers
      Button.add-row(@click="addServer") + Add server
  p.empty-list(v-if="pool.servers.length === 0") At least one server is required.
  ItemScope.route-card(v-for="(server, serverIndex) in pool.servers" :key="serverIndex" :index="serverIndex")
    header.cfg-route-card-heading
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
    .field-grid
      ConfigField(path="upstream_pools[].servers[].max_connections" label="Max connections")
        NumberInput(min="1" step="1" :model-value="server.max_connections ?? ''" placeholder="Unbounded" @update:model-value="server.max_connections = $event === '' ? null : Number($event)")
      ConfigField(path="upstream_pools[].servers[].dns_resolution" label="DNS resolution")
        Select(
          v-model="server.dns_resolution"
          :options="[{ value: 'on_connect', label: 'On connect' }, { value: 'startup', label: 'At startup' }]"
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
      ConfigField(path="upstream_pools[].health_check.startup" label="Initial startup state")
        Select(
          v-model="pool.health_check.startup"
          :options="[{ value: 'checking', label: 'Checking' }, { value: 'healthy', label: 'Healthy' }, { value: 'unhealthy', label: 'Unhealthy' }]"
        )
      ConfigField(path="upstream_pools[].health_check.fast_interval_ms" label="Fast transition interval (ms)")
        NumberInput(min="100" max="86400000" step="1" :model-value="pool.health_check.fast_interval_ms ?? ''" placeholder="Use interval" @update:model-value="pool.health_check.fast_interval_ms = $event === '' ? null : Number($event)")
      ConfigField(path="upstream_pools[].health_check.down_interval_ms" label="Down server interval (ms)")
        NumberInput(min="100" max="86400000" step="1" :model-value="pool.health_check.down_interval_ms ?? ''" placeholder="Use interval" @update:model-value="pool.health_check.down_interval_ms = $event === '' ? null : Number($event)")
    .field-grid(v-if="pool.health_check.type === 'http'")
      ConfigField(path="upstream_pools[].health_check.host" label="Host authority")
        Input(:model-value="pool.health_check.host ?? ''" @input="pool.health_check.host = nullableInput($event)")
      ConfigField(path="upstream_pools[].health_check.path" label="Probe path")
        Input(:model-value="pool.health_check.path ?? ''" @input="pool.health_check.path = nullableInput($event)")
      ConfigField(path="upstream_pools[].health_check.expected_status" label="Expected HTTP status")
        NumberInput(min="100" max="599" step="1" :model-value="pool.health_check.expected_status ?? ''" placeholder="200" @update:model-value="pool.health_check.expected_status = $event === '' ? null : Number($event)")
      ConfigField(path="upstream_pools[].health_check.http_version" label="Probe HTTP version")
        Select(
          :model-value="pool.health_check.http_version ?? ''"
          @update:model-value="setProbeHttpVersion($event)"
          :options="[{ value: '', label: 'Default (1.1)' }, { value: '1.1', label: 'HTTP/1.1' }, { value: '1.0', label: 'HTTP/1.0' }]"
        )
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
    ConfigGroup.object-block(path="upstream_pools[].passive_health.circuit_breaker" legend="Circuit breaker")
      Checkbox.enable-row(:model-value="pool.passive_health.circuit_breaker != null" @update:model-value="toggleCircuitBreaker") Trip server on high sliding-window failure ratio
      .field-grid(v-if="pool.passive_health.circuit_breaker")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.window_ms" label="Window (ms)")
          NumberInput(min="1000" step="1" v-model="pool.passive_health.circuit_breaker.window_ms")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.min_requests" label="Minimum requests")
          NumberInput(min="1" step="1" v-model="pool.passive_health.circuit_breaker.min_requests")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.failure_ratio_percent" label="Failure ratio (%)")
          NumberInput(min="1" max="100" step="1" v-model="pool.passive_health.circuit_breaker.failure_ratio_percent")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.open_ms" label="Open duration (ms)")
          NumberInput(min="1000" step="1" v-model="pool.passive_health.circuit_breaker.open_ms")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.half_open_max" label="Half-open trial concurrency")
          NumberInput(min="1" step="1" v-model="pool.passive_health.circuit_breaker.half_open_max")
        ConfigField(path="upstream_pools[].passive_health.circuit_breaker.half_open_successes" label="Half-open successes to close")
          NumberInput(min="1" step="1" v-model="pool.passive_health.circuit_breaker.half_open_successes")
    ConfigGroup.object-block(path="upstream_pools[].passive_health.outlier_detection" legend="Latency outlier detection")
      Checkbox.enable-row(:model-value="pool.passive_health.outlier_detection != null" @update:model-value="toggleOutlierDetection") Eject servers whose TTFB far exceeds peers
      .field-grid(v-if="pool.passive_health.outlier_detection")
        ConfigField(path="upstream_pools[].passive_health.outlier_detection.factor_percent" label="Threshold (% of median)")
          NumberInput(min="101" step="1" v-model="pool.passive_health.outlier_detection.factor_percent")
        ConfigField(path="upstream_pools[].passive_health.outlier_detection.min_samples" label="Min samples per interval")
          NumberInput(min="1" step="1" v-model="pool.passive_health.outlier_detection.min_samples")
        ConfigField(path="upstream_pools[].passive_health.outlier_detection.interval_ms" label="Evaluation interval (ms)")
          NumberInput(min="1000" step="1" v-model="pool.passive_health.outlier_detection.interval_ms")
        ConfigField(path="upstream_pools[].passive_health.outlier_detection.max_ejected_percent" label="Max ejected pool share (%)")
          NumberInput(min="1" max="100" step="1" v-model="pool.passive_health.outlier_detection.max_ejected_percent")
    ConfigGroup.object-block(path="upstream_pools[].passive_health.readmission" legend="Probe-based readmission")
      Checkbox.enable-row(:model-value="pool.passive_health.readmission != null" @update:model-value="toggleReadmission") Probe ejected servers before returning them to live traffic
      template(v-if="pool.passive_health.readmission")
        .field-grid
          ConfigField(path="upstream_pools[].passive_health.readmission.successes" label="Required consecutive successes")
            NumberInput(min="1" max="100" step="1" v-model="pool.passive_health.readmission.successes")
          ConfigField(path="upstream_pools[].passive_health.readmission.interval_ms" label="Probe interval (ms)")
            NumberInput(min="100" step="1" v-model="pool.passive_health.readmission.interval_ms")
          ConfigField(path="upstream_pools[].passive_health.readmission.timeout_ms" label="Probe timeout (ms)")
            NumberInput(min="10" step="1" v-model="pool.passive_health.readmission.timeout_ms")
        Checkbox.enable-row(:model-value="pool.passive_health.readmission.probe != null" @update:model-value="toggleReadmissionProbe") Use HTTP probe instead of TCP connect
        .field-grid(v-if="pool.passive_health.readmission.probe")
          ConfigField(path="upstream_pools[].passive_health.readmission.probe.path" label="Probe path")
            Input(v-model="pool.passive_health.readmission.probe.path" placeholder="/healthz")
          ConfigField(path="upstream_pools[].passive_health.readmission.probe.host" label="Probe Host header")
            Input(:model-value="pool.passive_health.readmission.probe.host ?? ''" placeholder="Optional" @update:model-value="pool.passive_health.readmission.probe.host = $event || null")
          ConfigField(path="upstream_pools[].passive_health.readmission.probe.expected_status" label="Expected status")
            NumberInput(min="100" max="599" step="1" :model-value="pool.passive_health.readmission.probe.expected_status ?? ''" placeholder="200" @update:model-value="pool.passive_health.readmission.probe.expected_status = $event === '' ? null : Number($event)")
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
ConfigGroup.object-block(path="upstream_pools[].http_versions" legend="Upstream HTTP versions and connection reuse")
  .field-grid
    ConfigField(path="upstream_pools[].http_versions.min" label="Minimum")
      select.ox-input(v-model="pool.http_versions.min" @change="normalizeHttpVersions('min')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
        option(value="3" :disabled="pool.tls === null") HTTP/3
    ConfigField(path="upstream_pools[].http_versions.max" label="Maximum")
      select.ox-input(v-model="pool.http_versions.max" @change="normalizeHttpVersions('max')")
        option(value="1.1") HTTP/1.1
        option(value="2" :disabled="pool.tls === null") HTTP/2
        option(value="3" :disabled="pool.tls === null") HTTP/3
    ConfigField(path="upstream_pools[].connection_reuse" label="Connection reuse")
      Select(
        v-model="pool.connection_reuse"
        :options="[{ value: 'safe', label: 'Safe (default)' }, { value: 'always', label: 'Always' }, { value: 'never', label: 'Never' }]"
      )
    ConfigField(path="upstream_pools[].connect_timeout_ms" label="Connect timeout (ms)")
      NumberInput(min="1" step="1" :model-value="pool.connect_timeout_ms ?? ''" placeholder="Inherit" @update:model-value="pool.connect_timeout_ms = $event === '' ? null : Number($event)")
    ConfigField(path="upstream_pools[].server_timeout_ms" label="Server I/O timeout (ms)")
      NumberInput(min="1" step="1" :model-value="pool.server_timeout_ms ?? ''" placeholder="Inherit" @update:model-value="pool.server_timeout_ms = $event === '' ? null : Number($event)")
    ConfigField(path="upstream_pools[].queue_timeout_ms" label="Capacity queue timeout (ms)")
      NumberInput(min="1" step="1" :model-value="pool.queue_timeout_ms ?? ''" placeholder="No wait" @update:model-value="pool.queue_timeout_ms = $event === '' ? null : Number($event)")
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
ConfigGroup.object-block(path="upstream_pools[].queue" legend="Priority queue scheduling")
  Checkbox.enable-row(:model-value="pool.queue != null" @update:model-value="toggleQueue") Enable class-based fair queuing when servers are saturated
  template(v-if="pool.queue")
    .field-grid
      ConfigField(path="upstream_pools[].queue.discipline" label="Discipline")
        Select(
          v-model="pool.queue.discipline"
          :options="[{ value: 'weighted', label: 'Weighted fair' }, { value: 'strict', label: 'Strict priority' }]"
        )
      ConfigField(path="upstream_pools[].queue.class_header" label="Class header")
        Input(:model-value="pool.queue.class_header ?? ''" placeholder="x-priority-class" @update:model-value="pool.queue.class_header = $event || null")
      ConfigField(path="upstream_pools[].queue.default_class" label="Default class")
        Input(:model-value="pool.queue.default_class ?? ''" placeholder="standard" @update:model-value="pool.queue.default_class = $event || null")
      ConfigField(path="upstream_pools[].queue.tenant_header" label="Tenant isolation header")
        Input(:model-value="pool.queue.tenant_header ?? ''" placeholder="Optional" @update:model-value="pool.queue.tenant_header = $event || null")
      ConfigField(path="upstream_pools[].queue.max_tenants" label="Max tracked tenants")
        NumberInput(min="1" step="1" v-model="pool.queue.max_tenants")
      ConfigField(path="upstream_pools[].queue.aging_ms" label="Priority aging interval (ms)")
        NumberInput(min="1" step="1" :model-value="pool.queue.aging_ms ?? ''" placeholder="Disabled" @update:model-value="pool.queue.aging_ms = $event === '' ? null : Number($event)")
    ConfigGroup.route-list(path="upstream_pools[].queue.classes")
      template(#legend)
        .cfg-route-heading
          legend Queue classes
          Button.add-row(@click="addQueueClass") + Add class
      ItemScope.route-card(v-for="(qc, qcIndex) in pool.queue.classes" :key="qcIndex" :index="qcIndex")
        header.cfg-route-card-heading
          strong Class {{ qcIndex + 1 }}
          Button.danger-link(variant="danger" small @click="removeQueueClass(qcIndex)") Remove
        .field-grid
          ConfigField(path="upstream_pools[].queue.classes[].name" label="Class name")
            Input(v-model="qc.name" placeholder="high")
          ConfigField(path="upstream_pools[].queue.classes[].weight" label="Weight")
            NumberInput(min="1" max="100" step="1" v-model="qc.weight")
ConfigGroup.object-block(path="upstream_pools[].registration" legend="Dynamic server registration")
  Checkbox.enable-row(:model-value="pool.registration != null" @update:model-value="toggleRegistration") Allow servers to self-register dynamically in this pool
  template(v-if="pool.registration")
    .field-grid
      ConfigField(path="upstream_pools[].registration.max_servers" label="Max registered servers")
        NumberInput(min="1" step="1" v-model="pool.registration.max_servers")
      ConfigField(path="upstream_pools[].registration.min_ttl_ms" label="Minimum lease TTL (ms)")
        NumberInput(min="1000" step="1" v-model="pool.registration.min_ttl_ms")
      ConfigField(path="upstream_pools[].registration.default_ttl_ms" label="Default lease TTL (ms)")
        NumberInput(min="1000" step="1" v-model="pool.registration.default_ttl_ms")
      ConfigField(path="upstream_pools[].registration.max_ttl_ms" label="Maximum lease TTL (ms)")
        NumberInput(min="1000" step="1" v-model="pool.registration.max_ttl_ms")
      ConfigField(path="upstream_pools[].registration.require_peer_address" label="Require peer IP to match registered address" check v-model="pool.registration.require_peer_address")
    StringListField(
      v-model="pool.registration.allowed_cidrs"
      label="Allowed endpoint CIDRs"
      item-label="CIDR"
      field-path="upstream_pools[].registration.allowed_cidrs"
      :max-items="64"
      @update:model-value="emit('changed')"
    )
ConfigGroup.object-block(path="upstream_pools[].canary" legend="Synthetic canary probe")
  Checkbox.enable-row(:model-value="pool.canary != null" @update:model-value="toggleCanary") Probe this pool periodically for canary latency and status alerts
  .field-grid(v-if="pool.canary")
    ConfigField(path="upstream_pools[].canary.path" label="Probe path")
      Input(v-model="pool.canary.path" placeholder="/healthz")
    ConfigField(path="upstream_pools[].canary.host" label="Probe Host header")
      Input(:model-value="pool.canary.host ?? ''" placeholder="Optional" @update:model-value="pool.canary.host = $event || null")
    ConfigField(path="upstream_pools[].canary.expected_status" label="Expected HTTP status")
      NumberInput(min="100" max="599" step="1" v-model="pool.canary.expected_status")
    ConfigField(path="upstream_pools[].canary.interval_ms" label="Interval (ms)")
      NumberInput(min="1000" step="1" v-model="pool.canary.interval_ms")
    ConfigField(path="upstream_pools[].canary.timeout_ms" label="Timeout (ms)")
      NumberInput(min="100" step="1" v-model="pool.canary.timeout_ms")
    ConfigField(path="upstream_pools[].canary.max_latency_ms" label="Latency warning ceiling (ms)")
      NumberInput(min="1" step="1" :model-value="pool.canary.max_latency_ms ?? ''" placeholder="Optional" @update:model-value="pool.canary.max_latency_ms = $event === '' ? null : Number($event)")
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
import StringListField from '../StringListField.vue'
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

const HTTP_VERSION_ORDER = { '1.1': 1, '2': 2, '3': 3 } as const

function normalizeHttpVersions(changed: 'min' | 'max'): void {
  const versions = props.pool.http_versions
  if (props.pool.tls === null) {
    versions.min = '1.1'
    versions.max = '1.1'
  } else if (changed === 'min' && HTTP_VERSION_ORDER[versions.min] > HTTP_VERSION_ORDER[versions.max]) {
    versions.max = versions.min
  } else if (changed === 'max' && HTTP_VERSION_ORDER[versions.max] < HTTP_VERSION_ORDER[versions.min]) {
    versions.min = versions.max
  }
}

function setProbeHttpVersion(value: string): void {
  if (!props.pool.health_check) return
  props.pool.health_check.http_version = value === '1.0' || value === '1.1' ? value : null
}

function toggleCircuitBreaker(enabled: boolean): void {
  if (!props.pool.passive_health) return
  props.pool.passive_health.circuit_breaker = enabled
    ? { window_ms: 30_000, min_requests: 20, failure_ratio_percent: 50, open_ms: 30_000, half_open_max: 2, half_open_successes: 2 }
    : null
}

function toggleOutlierDetection(enabled: boolean): void {
  if (!props.pool.passive_health) return
  props.pool.passive_health.outlier_detection = enabled
    ? { factor_percent: 300, min_samples: 20, interval_ms: 10_000, max_ejected_percent: 50 }
    : null
}

function toggleReadmission(enabled: boolean): void {
  if (!props.pool.passive_health) return
  props.pool.passive_health.readmission = enabled
    ? { successes: 2, interval_ms: 5_000, timeout_ms: 1_000, probe: null }
    : null
}

function toggleReadmissionProbe(enabled: boolean): void {
  if (!props.pool.passive_health?.readmission) return
  props.pool.passive_health.readmission.probe = enabled
    ? { path: '/healthz', host: null, expected_status: 200 }
    : null
}

function toggleQueue(enabled: boolean): void {
  props.pool.queue = enabled
    ? {
        classes: [{ name: 'high', weight: 4 }, { name: 'standard', weight: 1 }],
        discipline: 'weighted',
        class_header: 'x-priority-class',
        default_class: 'standard',
        tenant_header: null,
        max_tenants: 64,
        aging_ms: null,
      }
    : null
}

function addQueueClass(): void {
  props.pool.queue?.classes.push({ name: '', weight: 1 })
  emit('changed')
}

function removeQueueClass(index: number): void {
  props.pool.queue?.classes.splice(index, 1)
  emit('changed')
}

function toggleRegistration(enabled: boolean): void {
  props.pool.registration = enabled
    ? {
        max_servers: 64,
        min_ttl_ms: 5_000,
        default_ttl_ms: 30_000,
        max_ttl_ms: 300_000,
        allowed_cidrs: ['127.0.0.0/8', '10.0.0.0/8'],
        require_peer_address: false,
      }
    : null
}

function toggleCanary(enabled: boolean): void {
  props.pool.canary = enabled
    ? { path: '/healthz', host: null, expected_status: 200, interval_ms: 15_000, timeout_ms: 3_000, max_latency_ms: 500 }
    : null
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
