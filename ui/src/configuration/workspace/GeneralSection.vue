<template lang="pug">
SectionHeading(eyebrow="Root object" title="General" path="Config")
.field-grid
  ConfigField(path="version" label="Version")
    NumberInput(min="1" step="1" v-model="config.version")
    small Current canonical schema version.
  ConfigField(path="log_filter" label="Log filter")
    Input(:model-value="config.log_filter ?? ''" placeholder="info" @update:model-value="setOptionalString('log_filter', $event)")
    small Tracing filter directive (e.g. info, oxiroute=debug).
  ConfigField(path="cpu_level" label="CPU dispatch level")
    Input(:model-value="config.cpu_level ?? ''" placeholder="auto" @update:model-value="setOptionalString('cpu_level', $event)")
    small Optional SIMD dispatch override (e.g. auto, scalar, avx2, avx512).

NullableLimitField(
  v-model="config.max_connections"
  :default-value="65536"
  field-path="max_connections"
  legend="Process connection limit"
  input-label="Maximum active connections across all listeners"
)

ConfigGroup.object-block(path="listener_workers" legend="Workers, data plane and HTTP/3")
  .field-grid
    ConfigField(path="listener_workers" label="Listener workers")
      NumberInput(min="1" step="1" :model-value="config.listener_workers ?? ''" placeholder="Auto" @update:model-value="setOptionalNumber('listener_workers', $event)")
    ConfigField(path="http_data_plane" label="HTTP data plane")
      Select(
        :model-value="config.http_data_plane ?? ''"
        @update:model-value="setOptionalEnum('http_data_plane', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'epoll', label: 'epoll' }, { value: 'io_uring', label: 'io_uring' }, { value: 'tokio', label: 'Tokio' }]"
      )
    ConfigField(path="http_fast_lane" label="HTTP fast lane")
      Select(
        :model-value="optionalBool(config.http_fast_lane)"
        @update:model-value="setOptionalBool('http_fast_lane', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'true', label: 'Enabled' }, { value: 'false', label: 'Disabled' }]"
      )
    ConfigField(path="connection_scheduling" label="Connection scheduling")
      Select(
        :model-value="config.connection_scheduling ?? ''"
        @update:model-value="setOptionalEnum('connection_scheduling', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'auto', label: 'Auto' }, { value: 'work_stealing', label: 'Work stealing' }, { value: 'aged_per_worker', label: 'Aged per worker' }]"
      )
    ConfigField(path="connection_age_ms" label="Connection age threshold (ms)")
      NumberInput(min="1" step="1" :model-value="config.connection_age_ms ?? ''" placeholder="Default" @update:model-value="setOptionalNumber('connection_age_ms', $event)")
    ConfigField(path="worker_scaling" label="Worker scaling")
      Select(
        :model-value="config.worker_scaling ?? ''"
        @update:model-value="setOptionalEnum('worker_scaling', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'static', label: 'Static' }, { value: 'dynamic', label: 'Dynamic' }]"
      )
    ConfigField(path="min_active_workers" label="Minimum active workers")
      NumberInput(min="1" step="1" :model-value="config.min_active_workers ?? ''" placeholder="Default" @update:model-value="setOptionalNumber('min_active_workers', $event)")
    ConfigField(path="target_busy_percent" label="Target busy percent")
      NumberInput(min="1" max="100" step="1" :model-value="config.target_busy_percent ?? ''" placeholder="Default" @update:model-value="setOptionalNumber('target_busy_percent', $event)")
    ConfigField(path="scale_down_delay_ms" label="Scale-down delay (ms)")
      NumberInput(min="1" step="1" :model-value="config.scale_down_delay_ms ?? ''" placeholder="Default" @update:model-value="setOptionalNumber('scale_down_delay_ms', $event)")
    ConfigField(path="tls_handshake_threads" label="TLS handshake threads")
      NumberInput(min="1" step="1" :model-value="config.tls_handshake_threads ?? ''" placeholder="Default" @update:model-value="setOptionalNumber('tls_handshake_threads', $event)")
    ConfigField(path="http3_congestion_control" label="HTTP/3 congestion control")
      Select(
        :model-value="config.http3_congestion_control ?? ''"
        @update:model-value="setOptionalEnum('http3_congestion_control', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'cubic', label: 'CUBIC' }, { value: 'bbr', label: 'BBR' }]"
      )
    ConfigField(path="http3_segmentation_offload" label="HTTP/3 GSO segmentation offload")
      Select(
        :model-value="optionalBool(config.http3_segmentation_offload)"
        @update:model-value="setOptionalBool('http3_segmentation_offload', $event)"
        :options="[{ value: '', label: 'Runtime default' }, { value: 'true', label: 'Enabled' }, { value: 'false', label: 'Disabled' }]"
      )

ConfigGroup.object-block(path="retirement" legend="Generation retirement")
  Checkbox.enable-row(:model-value="config.retirement != null" @update:model-value="toggleRetirement") Configure generation retirement bounds
  .field-grid(v-if="config.retirement")
    ConfigField(path="retirement.drain_deadline_ms" label="Drain deadline (ms)")
      NumberInput(min="1" step="1" :model-value="config.retirement.drain_deadline_ms ?? ''" placeholder="Unlimited" @update:model-value="config.retirement.drain_deadline_ms = $event === '' ? null : Number($event)")
    ConfigField(path="retirement.max_retired_generations" label="Max retired generations")
      NumberInput(min="1" step="1" v-model="config.retirement.max_retired_generations")

ConfigGroup.object-block(path="audit" legend="Audit log")
  Checkbox.enable-row(:model-value="config.audit != null" @update:model-value="toggleAudit") Customize audit log retention
  .field-grid(v-if="config.audit")
    ConfigField(path="audit.dir" label="Audit directory")
      Input(:model-value="config.audit.dir ?? ''" placeholder="Default alongside config" @update:model-value="config.audit.dir = $event || undefined")
    ConfigField(path="audit.max_records" label="Max in-memory records")
      NumberInput(min="1" step="1" :model-value="config.audit.max_records ?? ''" placeholder="Default" @update:model-value="config.audit.max_records = parseOptInt($event)")
    ConfigField(path="audit.max_record_bytes" label="Max record bytes")
      NumberInput(min="1" step="1" :model-value="config.audit.max_record_bytes ?? ''" placeholder="Default" @update:model-value="config.audit.max_record_bytes = parseOptInt($event)")
    ConfigField(path="audit.max_file_bytes" label="Max file bytes")
      NumberInput(min="1" step="1" :model-value="config.audit.max_file_bytes ?? ''" placeholder="Default" @update:model-value="config.audit.max_file_bytes = parseOptInt($event)")
    ConfigField(path="audit.max_total_bytes" label="Max total bytes")
      NumberInput(min="1" step="1" :model-value="config.audit.max_total_bytes ?? ''" placeholder="Default" @update:model-value="config.audit.max_total_bytes = parseOptInt($event)")
    ConfigField(path="audit.max_rotated_files" label="Max rotated files")
      NumberInput(min="1" step="1" :model-value="config.audit.max_rotated_files ?? ''" placeholder="Default" @update:model-value="config.audit.max_rotated_files = parseOptInt($event)")

ConfigGroup.object-block(path="config_history" legend="Configuration history")
  Checkbox.enable-row(:model-value="config.config_history != null" @update:model-value="toggleConfigHistory") Customize on-disk revision history bounds
  .field-grid(v-if="config.config_history")
    ConfigField(path="config_history.max_entries" label="Max stored revisions")
      NumberInput(min="1" step="1" :model-value="config.config_history.max_entries ?? ''" placeholder="Default" @update:model-value="config.config_history.max_entries = parseOptInt($event)")
    ConfigField(path="config_history.max_total_bytes" label="Max total history bytes")
      NumberInput(min="1" step="1" :model-value="config.config_history.max_total_bytes ?? ''" placeholder="Default" @update:model-value="config.config_history.max_total_bytes = parseOptInt($event)")

ConfigGroup.object-block(path="client_tracking" legend="Client tracking")
  Checkbox.enable-row(:model-value="config.client_tracking != null" @update:model-value="toggleClientTracking") Customize live client and connection tracking
  .field-grid(v-if="config.client_tracking")
    ConfigField(path="client_tracking.enabled" label="Track active clients" check :model-value="config.client_tracking.enabled ?? true" @update:model-value="config.client_tracking.enabled = Boolean($event)")
    ConfigField(path="client_tracking.max_clients" label="Max tracked clients")
      NumberInput(min="1" step="1" :model-value="config.client_tracking.max_clients ?? ''" placeholder="Default" @update:model-value="config.client_tracking.max_clients = parseOptInt($event)")
    ConfigField(path="client_tracking.ipv6_prefix" label="IPv6 aggregation prefix")
      NumberInput(min="1" max="128" step="1" :model-value="config.client_tracking.ipv6_prefix ?? ''" placeholder="64" @update:model-value="config.client_tracking.ipv6_prefix = parseOptInt($event)")

ConfigGroup.object-block(path="registry" legend="Dynamic server registry")
  Checkbox.enable-row(:model-value="config.registry != null" @update:model-value="toggleRegistry") Enable dynamic server registration registry
  template(v-if="config.registry")
    .field-grid
      ConfigField(path="registry.state_dir" label="Registry state directory")
        Input(v-model="config.registry.state_dir" placeholder="/var/lib/oxiroute/registry")
      ConfigField(path="registry.tokens_file" label="Registry tokens file")
        Input(:model-value="config.registry.tokens_file ?? ''" placeholder="Optional path" @update:model-value="config.registry.tokens_file = $event || null")
    ConfigGroup.route-list(path="registry.endpoints")
      template(#legend)
        .cfg-route-heading
          legend Registry endpoints
          Button.add-row(@click="addRegistryEndpoint") + Add endpoint
      ItemScope.route-card(v-for="(endpoint, idx) in config.registry.endpoints" :key="idx" :index="idx")
        header.cfg-route-card-heading
          strong Endpoint {{ idx + 1 }}
          Button.danger-link(variant="danger" small @click="config.registry.endpoints.splice(idx, 1)") Remove
        .field-grid
          ConfigField(path="registry.endpoints[].type" label="Transport")
            Select(
              :model-value="endpoint.type"
              @update:model-value="changeRegistryEndpointType(idx, $event)"
              :options="[{ value: 'unix', label: 'Unix socket' }, { value: 'tcp', label: 'TCP bind' }]"
            )
          template(v-if="endpoint.type === 'unix'")
            ConfigField(path="registry.endpoints[].path" label="Socket path")
              Input(v-model="endpoint.path" placeholder="/run/oxiroute/registry.sock")
            ConfigField(path="registry.endpoints[].mode" label="File mode (decimal)")
              NumberInput(min="0" max="511" step="1" v-model="endpoint.mode")
            ConfigField(path="registry.endpoints[].role" label="Role")
              Select(
                v-model="endpoint.role"
                :options="[{ value: 'register', label: 'Register (read/write)' }, { value: 'read', label: 'Read only' }]"
              )
            StringListField(
              v-model="endpoint.pools"
              label="Allowed pools (empty = all)"
              item-label="pool"
              field-path="registry.endpoints[].pools"
              :max-items="64"
            )
          template(v-else)
            ConfigField(path="registry.endpoints[].bind" label="TCP bind address")
              Input(v-model="endpoint.bind" placeholder="127.0.0.1:9444")
            StringListField(
              v-model="endpoint.allow_cidrs"
              label="Allowed client CIDRs"
              item-label="CIDR"
              field-path="registry.endpoints[].allow_cidrs"
              :max-items="64"
            )
</template>

<script setup lang="ts" vapor>
import Button from '#design/button/Button.vue'
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import type { CanonicalConfig } from '../../config'
import StringListField from '../../StringListField.vue'
import NullableLimitField from '../NullableLimitField.vue'
import ConfigField from '../fields/ConfigField.vue'
import ConfigGroup from '../fields/ConfigGroup.vue'
import ItemScope from '../fields/ItemScope.vue'
import SectionHeading from './SectionHeading.vue'

const props = defineProps<{ config: CanonicalConfig }>()

function setOptionalString(key: 'log_filter' | 'cpu_level', value: string): void {
  if (value.trim()) props.config[key] = value
  else delete props.config[key]
}

function setOptionalNumber(
  key: 'listener_workers' | 'connection_age_ms' | 'min_active_workers' | 'target_busy_percent' | 'scale_down_delay_ms' | 'tls_handshake_threads',
  value: number | string,
): void {
  if (value === '') delete props.config[key]
  else props.config[key] = Number(value)
}

function setOptionalEnum(
  key: 'http_data_plane' | 'connection_scheduling' | 'worker_scaling' | 'http3_congestion_control',
  value: string,
): void {
  if (!value) delete props.config[key]
  else (props.config as unknown as Record<string, unknown>)[key] = value
}

function optionalBool(value: boolean | undefined): string {
  return value === undefined ? '' : value ? 'true' : 'false'
}

function setOptionalBool(key: 'http_fast_lane' | 'http3_segmentation_offload', value: string): void {
  if (!value) delete props.config[key]
  else props.config[key] = value === 'true'
}

function parseOptInt(value: number | string): number | undefined {
  return value === '' ? undefined : Number(value)
}

function toggleRetirement(enabled: boolean): void {
  props.config.retirement = enabled ? { drain_deadline_ms: 30_000, max_retired_generations: 4 } : undefined
}

function toggleAudit(enabled: boolean): void {
  props.config.audit = enabled ? {} : undefined
}

function toggleConfigHistory(enabled: boolean): void {
  props.config.config_history = enabled ? { max_entries: 50 } : undefined
}

function toggleClientTracking(enabled: boolean): void {
  props.config.client_tracking = enabled ? { enabled: true } : undefined
}

function toggleRegistry(enabled: boolean): void {
  props.config.registry = enabled
    ? { state_dir: '/var/lib/oxiroute/registry', tokens_file: null, endpoints: [] }
    : null
}

function addRegistryEndpoint(): void {
  props.config.registry?.endpoints.push({
    type: 'unix',
    path: '/run/oxiroute/registry.sock',
    mode: 0o660,
    role: 'register',
    pools: [],
  })
}

function changeRegistryEndpointType(index: number, type: string): void {
  if (!props.config.registry) return
  props.config.registry.endpoints[index] = type === 'tcp'
    ? { type: 'tcp', bind: '127.0.0.1:9444', allow_cidrs: ['127.0.0.0/8'] }
    : { type: 'unix', path: '/run/oxiroute/registry.sock', mode: 0o660, role: 'register', pools: [] }
}
</script>
