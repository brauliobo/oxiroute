<template lang="pug">
.action-editor
  .field-grid
    ConfigField(path="http_services[].routes[].action.status" label="HTTP status")
      NumberInput(min="200" max="599" step="1" v-model="action.status")
    ConfigField.field-wide(path="http_services[].routes[].action.body" label="Response body")
      textarea.ox-input(
        v-model="action.body"
        rows="5"
        maxlength="65536"
        :aria-describedby="bodyHintId"
        :aria-invalid="bodyForbiddenWithContent ? 'true' : undefined"
      )
      small(:id="bodyHintId") Status 204, 205, and 304 require an empty body. Maximum 65,536 UTF-8 bytes.
      small.field-warning(v-if="bodyForbiddenWithContent" role="alert") Clear the body for status {{ action.status }}.
  ConfigGroup.route-list(path="http_services[].routes[].action.headers")
    template(#legend)
      .cfg-route-heading
        legend Response headers
        Button.add-row(:disabled="action.headers.length >= 32" :title="action.headers.length >= 32 ? 'The server allows at most 32 fixed response headers.' : undefined" @click="addHeader") + Add header
    ItemScope.route-card(v-for="(header, headerIndex) in action.headers" :key="headerIndex" :index="headerIndex")
      header.cfg-route-card-heading
        strong Header {{ headerIndex + 1 }}
        Button.danger-link(variant="danger" small :aria-label="`Remove fixed response header ${headerIndex + 1}`" @click="removeHeader(headerIndex)") Remove
      .field-grid
        ConfigField(path="http_services[].routes[].action.headers[].name" label="Header name")
          Input(v-model="header.name")
        ConfigField(path="http_services[].routes[].action.headers[].value" label="Header value")
          Input(v-model="header.value")
        ConfigField(path="http_services[].routes[].action.headers[].always" label="Add on every response status" check v-model="header.always")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Button from '#design/button/Button.vue'
import { computed, useId } from 'vue'
import type { HttpFixedResponseActionConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{ action: HttpFixedResponseActionConfig }>()
const emit = defineEmits<{ changed: [] }>()
const bodyHintId = useId()
const bodyForbiddenWithContent = computed(
  () => [204, 205, 304].includes(props.action.status) && props.action.body.length > 0,
)

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
