<template lang="pug">
header.cfg-form-heading
  div
    p.cfg-eyebrow HTTP dispatch
    h3 {{ service.name || 'Unnamed HTTP service' }}
  Button.danger-button(variant="danger" @click="emit('remove')") Remove service
.field-grid
  ConfigField(path="http_services[].name" label="Stable name")
    Input(v-model="service.name")
  ConfigField(path="http_services[].upstream_io_timeout_ms" label="Upstream I/O timeout (ms)")
    NumberInput(min="1" step="1" v-model="service.upstream_io_timeout_ms")
  ConfigField(path="http_services[].drain_deadline_ms" label="Drain deadline (ms)")
    NumberInput(min="1" step="1" :model-value="service.drain_deadline_ms ?? ''" placeholder="Inherit retirement deadline" @update:model-value="service.drain_deadline_ms = $event === '' ? null : Number($event)")
  ConfigField(path="http_services[].automatic_response_headers" label="Generate Date and Connection response headers" check v-model="service.automatic_response_headers")
NullableLimitField(
  v-model="service.max_request_body_bytes"
  :default-value="10485760"
  field-path="http_services[].max_request_body_bytes"
  legend="Request body limit"
  input-label="Maximum request body (bytes)"
)
ConfigGroup.object-block(path="http_services[].access_log" legend="HTTP access log")
  .field-grid
    ConfigField(path="http_services[].access_log.type" label="Access log mode")
      Select(
        :model-value="service.access_log?.type ?? 'default'"
        @update:model-value="setAccessLog($event)"
        :options="[{ value: 'default', label: 'Runtime default' }, { value: 'disabled', label: 'Disabled' }, { value: 'file', label: 'File' }]"
      )
    ConfigField(v-if="service.access_log?.type === 'file'" path="http_services[].access_log.path" label="Log file path")
      Input(v-model="service.access_log.path" placeholder="/var/log/oxiroute/access.log")
ConfigGroup.object-block(path="http_services[].gzip" legend="Dynamic Gzip compression")
  Checkbox.enable-row(:model-value="service.gzip !== null" @update:model-value="toggleGzip") Compress eligible HTTP responses with Gzip
  template(v-if="service.gzip")
    .field-grid
      ConfigField(path="http_services[].gzip.level" label="Compression level (1-9)")
        NumberInput(min="1" max="9" step="1" v-model="service.gzip.level")
      ConfigField(path="http_services[].gzip.min_length_bytes" label="Minimum length (bytes)")
        NumberInput(min="0" step="1" v-model="service.gzip.min_length_bytes")
      ConfigField(path="http_services[].gzip.min_http_version" label="Minimum HTTP version")
        Select(
          v-model="service.gzip.min_http_version"
          :options="[{ value: '1.1', label: 'HTTP/1.1' }, { value: '1.0', label: 'HTTP/1.0' }]"
        )
      ConfigField(path="http_services[].gzip.vary" label="Emit Vary: Accept-Encoding" check v-model="service.gzip.vary")
      ConfigField(path="http_services[].gzip.disable_on_via" label="Skip when Via header is present" check v-model="service.gzip.disable_on_via")
    StringListField(
      v-model="service.gzip.content_types"
      label="Compressible content types"
      item-label="content type"
      field-path="http_services[].gzip.content_types"
      :max-items="64"
      @update:model-value="emit('changed')"
    )
ConfigGroup.route-list(path="http_services[].routes")
  template(#legend)
    .cfg-route-heading
      legend Routes
      Button.add-row(@click="addRoute") + Add route
  p.empty-list(v-if="service.routes.length === 0") At least one route is required.
  HttpRouteEditor(
    v-for="(route, routeIndex) in service.routes"
    :key="routeIndex"
    :route="route"
    :index="routeIndex"
    :pool-names="poolNames"
    :cache-store-names="cacheStoreNames"
    @changed="emit('changed')"
    @remove="removeRoute(routeIndex)"
  )
</template>

<script setup lang="ts" vapor>
import Checkbox from '#design/field/Checkbox.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import type { HttpServiceConfig } from '../config'
import StringListField from '../StringListField.vue'
import HttpRouteEditor from './HttpRouteEditor.vue'
import NullableLimitField from './NullableLimitField.vue'
import { defaultHttpRoute } from './httpDefaults'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

const props = defineProps<{
  service: HttpServiceConfig
  poolNames: string[]
  cacheStoreNames: string[]
}>()

const emit = defineEmits<{
  changed: []
  remove: []
}>()

function addRoute(): void {
  props.service.routes.push(defaultHttpRoute())
  emit('changed')
}

function removeRoute(index: number): void {
  props.service.routes.splice(index, 1)
  emit('changed')
}

function setAccessLog(mode: string): void {
  props.service.access_log = mode === 'disabled'
    ? { type: 'disabled' }
    : mode === 'file'
      ? { type: 'file', path: '/var/log/oxiroute/access.log' }
      : null
  emit('changed')
}

function toggleGzip(enabled: boolean): void {
  props.service.gzip = enabled
    ? {
        level: 4,
        content_types: ['text/plain', 'text/css', 'application/json', 'application/javascript'],
        min_length_bytes: 1024,
        min_http_version: '1.1',
        disable_on_via: false,
        vary: true,
      }
    : null
  emit('changed')
}
</script>
