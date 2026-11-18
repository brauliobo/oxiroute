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
  ConfigField(path="http_services[].automatic_response_headers" label="Generate Date and Connection response headers" check v-model="service.automatic_response_headers")
NullableLimitField(
  v-model="service.max_request_body_bytes"
  :default-value="10485760"
  field-path="http_services[].max_request_body_bytes"
  legend="Request body limit"
  input-label="Maximum request body (bytes)"
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
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Button from '#design/button/Button.vue'
import type { HttpServiceConfig } from '../config'
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
</script>
