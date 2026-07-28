<template lang="pug">
.action-editor
  .field-grid
    label.field(data-field="http_services[].routes[].action.status")
      span Redirect status
      select(v-model.number="action.status")
        option(:value="301") 301 Permanent
        option(:value="302") 302 Temporary
        option(:value="307") 307 Temporary, preserve method
        option(:value="308") 308 Permanent, preserve method
  fieldset.object-block(data-field="http_services[].routes[].action.location")
    legend Redirect location
    .field-grid
      label.field(data-field="http_services[].routes[].action.location.kind")
        span Location kind
        select(:value="action.location.kind" @change="changeLocation")
          option(value="literal") Literal location
          option(value="request_template") Request template
      label.field.field-wide(data-field="http_services[].routes[].action.location.value")
        span Location value
        input(type="text" v-model="action.location.value" maxlength="2048" :aria-describedby="locationHintId")
        small(:id="locationHintId") {{ locationHint }}
      label.field(v-if="action.location.kind === 'request_template'" data-field="http_services[].routes[].action.location.nginx_host_fallback")
        span Missing-Host fallback
        input(type="text" :value="action.location.nginx_host_fallback ?? ''" placeholder="localhost" @input="setNginxHostFallback")
  fieldset.route-list(data-field="http_services[].routes[].action.headers")
    .route-heading
      legend Response headers
      button.add-row(type="button" :disabled="action.headers.length >= 32" @click="addHeader") + Add header
    article.route-card(v-for="(header, headerIndex) in action.headers" :key="headerIndex")
      header.route-card-heading
        strong Header {{ headerIndex + 1 }}
        button.danger-link(type="button" :aria-label="`Remove redirect header ${headerIndex + 1}`" @click="removeHeader(headerIndex)") Remove
      .field-grid
        label.field(data-field="http_services[].routes[].action.headers[].name")
          span Header name
          input(type="text" v-model="header.name")
        label.field(data-field="http_services[].routes[].action.headers[].value")
          span Header value
          input(type="text" v-model="header.value")
        label.enable-row(data-field="http_services[].routes[].action.headers[].always")
          input(type="checkbox" v-model="header.always")
          span Add on every response status
</template>

<script setup lang="ts">
import { computed, useId } from 'vue'
import type { HttpRedirectActionConfig, HttpRedirectLocationConfig } from '../config'
import { defaultRedirectLocation } from './httpDefaults'

const props = defineProps<{ action: HttpRedirectActionConfig }>()
const emit = defineEmits<{ changed: [] }>()
const locationHintId = useId()
const locationHint = computed(() => props.action.location.kind === 'request_template'
  ? 'Only $scheme, $host, and $request_uri variables are supported.'
  : 'A literal Location header value of at most 2,048 bytes.')

function changeLocation(event: Event): void {
  props.action.location = defaultRedirectLocation(
    (event.target as HTMLSelectElement).value as HttpRedirectLocationConfig['kind'],
  )
}

function setNginxHostFallback(event: Event): void {
  if (props.action.location.kind === 'request_template') {
    props.action.location.nginx_host_fallback = (event.target as HTMLInputElement).value || null
  }
}

function addHeader(): void {
  if (props.action.headers.length >= 32) return
  props.action.headers.push({ name: '', value: '', always: false })
  emit('changed')
}

function removeHeader(index: number): void {
  props.action.headers.splice(index, 1)
  emit('changed')
}
</script>
