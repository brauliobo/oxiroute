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
</template>

<script setup lang="ts">
import { computed, useId } from 'vue'
import type { HttpRedirectActionConfig, HttpRedirectLocationConfig } from '../config'
import { defaultRedirectLocation } from './httpDefaults'

const props = defineProps<{ action: HttpRedirectActionConfig }>()
const locationHintId = useId()
const locationHint = computed(() => props.action.location.kind === 'request_template'
  ? 'Only $scheme, $host, and $request_uri variables are supported.'
  : 'A literal Location header value of at most 2,048 bytes.')

function changeLocation(event: Event): void {
  props.action.location = defaultRedirectLocation(
    (event.target as HTMLSelectElement).value as HttpRedirectLocationConfig['kind'],
  )
}
</script>
