<template lang="pug">
.action-editor
  .field-grid
    label.field(data-field="http_services[].routes[].action.status")
      span HTTP status
      input(type="number" min="200" max="599" step="1" v-model.number="action.status")
    label.field.field-wide(data-field="http_services[].routes[].action.body")
      span Response body
      textarea(
        v-model="action.body"
        rows="5"
        maxlength="65536"
        :aria-describedby="bodyHintId"
        :aria-invalid="bodyForbiddenWithContent ? 'true' : undefined"
      )
      small(:id="bodyHintId") Status 204, 205, and 304 require an empty body. Maximum 65,536 UTF-8 bytes.
      small.field-warning(v-if="bodyForbiddenWithContent" role="alert") Clear the body for status {{ action.status }}.
  fieldset.route-list(data-field="http_services[].routes[].action.headers")
    .route-heading
      legend Response headers
      button.add-row(type="button" :disabled="action.headers.length >= 32" :title="action.headers.length >= 32 ? 'The server allows at most 32 fixed response headers.' : undefined" @click="addHeader") + Add header
    article.route-card(v-for="(header, headerIndex) in action.headers" :key="headerIndex")
      header.route-card-heading
        strong Header {{ headerIndex + 1 }}
        button.danger-link(type="button" :aria-label="`Remove fixed response header ${headerIndex + 1}`" @click="removeHeader(headerIndex)") Remove
      .field-grid
        label.field(data-field="http_services[].routes[].action.headers[].name")
          span Header name
          input(type="text" v-model="header.name")
        label.field(data-field="http_services[].routes[].action.headers[].value")
          span Header value
          input(type="text" v-model="header.value")
</template>

<script setup lang="ts">
import { computed, useId } from 'vue'
import type { HttpFixedResponseActionConfig } from '../config'

const props = defineProps<{ action: HttpFixedResponseActionConfig }>()
const emit = defineEmits<{ changed: [] }>()
const bodyHintId = useId()
const bodyForbiddenWithContent = computed(
  () => [204, 205, 304].includes(props.action.status) && props.action.body.length > 0,
)

function addHeader(): void {
  if (props.action.headers.length >= 32) return
  props.action.headers.push({ name: '', value: '' })
  emit('changed')
}

function removeHeader(index: number): void {
  props.action.headers.splice(index, 1)
  emit('changed')
}
</script>
