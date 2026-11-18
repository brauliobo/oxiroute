<template lang="pug">
.action-editor
  .field-grid
    ConfigField(path="http_services[].routes[].action.status" label="Redirect status")
      select.ox-input(v-model.number="action.status")
        option(:value="301") 301 Permanent
        option(:value="302") 302 Temporary
        option(:value="307") 307 Temporary, preserve method
        option(:value="308") 308 Permanent, preserve method
  ConfigGroup.object-block(path="http_services[].routes[].action.location" legend="Redirect location")
    .field-grid
      ConfigField(path="http_services[].routes[].action.location.kind" label="Location kind")
        Select(
          :model-value="action.location.kind" @change="changeLocation"
          :options="[{ value: 'literal', label: 'Literal location' }, { value: 'request_template', label: 'Request template' }]"
        )
      ConfigField.field-wide(path="http_services[].routes[].action.location.value" label="Location value")
        Input(v-model="action.location.value" maxlength="2048" :aria-describedby="locationHintId")
        small(:id="locationHintId") {{ locationHint }}
      ConfigField(v-if="action.location.kind === 'request_template'" path="http_services[].routes[].action.location.nginx_host_fallback" label="Missing-Host fallback")
        Input(:model-value="action.location.nginx_host_fallback ?? ''" placeholder="localhost" @input="setNginxHostFallback")
  ConfigGroup.route-list(path="http_services[].routes[].action.headers")
    template(#legend)
      .cfg-route-heading
        legend Response headers
        Button.add-row(:disabled="action.headers.length >= 32" @click="addHeader") + Add header
    ItemScope.route-card(v-for="(header, headerIndex) in action.headers" :key="headerIndex" :index="headerIndex")
      header.cfg-route-card-heading
        strong Header {{ headerIndex + 1 }}
        Button.danger-link(variant="danger" small :aria-label="`Remove redirect header ${headerIndex + 1}`" @click="removeHeader(headerIndex)") Remove
      .field-grid
        ConfigField(path="http_services[].routes[].action.headers[].name" label="Header name")
          Input(v-model="header.name")
        ConfigField(path="http_services[].routes[].action.headers[].value" label="Header value")
          Input(v-model="header.value")
        ConfigField(path="http_services[].routes[].action.headers[].always" label="Add on every response status" check v-model="header.always")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Button from '#design/button/Button.vue'
import { computed, useId } from 'vue'
import type { HttpRedirectActionConfig, HttpRedirectLocationConfig } from '../config'
import { defaultRedirectLocation } from './httpDefaults'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

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
