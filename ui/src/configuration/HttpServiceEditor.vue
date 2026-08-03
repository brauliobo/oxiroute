<template lang="pug">
header.form-heading
  div
    p.eyebrow HTTP dispatch
    h3 {{ service.name || 'Unnamed HTTP service' }}
  button.danger-button(type="button" @click="$emit('remove')") Remove service
.field-grid
  label.field(data-field="http_services[].name")
    span Stable name
    input(type="text" v-model="service.name")
  label.field(data-field="http_services[].upstream_io_timeout_ms")
    span Upstream I/O timeout (ms)
    input(type="number" min="1" step="1" v-model.number="service.upstream_io_timeout_ms")
  label.field.checkbox(data-field="http_services[].automatic_response_headers")
    input(type="checkbox" v-model="service.automatic_response_headers")
    span Generate Date and Connection response headers
NullableLimitField(
  v-model="service.max_request_body_bytes"
  :default-value="10485760"
  field-path="http_services[].max_request_body_bytes"
  legend="Request body limit"
  input-label="Maximum request body (bytes)"
)
fieldset.route-list(data-field="http_services[].routes")
  .route-heading
    legend Routes
    button.add-row(type="button" @click="addRoute") + Add route
  p.empty-list(v-if="service.routes.length === 0") At least one route is required.
  HttpRouteEditor(
    v-for="(route, routeIndex) in service.routes"
    :key="routeIndex"
    :route="route"
    :index="routeIndex"
    :pool-names="poolNames"
    :cache-store-names="cacheStoreNames"
    @changed="$emit('changed')"
    @remove="removeRoute(routeIndex)"
  )
</template>

<script setup lang="ts">
import type { HttpServiceConfig } from '../config'
import HttpRouteEditor from './HttpRouteEditor.vue'
import NullableLimitField from './NullableLimitField.vue'
import { defaultHttpRoute } from './httpDefaults'

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
