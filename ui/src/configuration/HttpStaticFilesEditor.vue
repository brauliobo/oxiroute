<template lang="pug">
.action-editor
  .field-grid
    ConfigField.field-wide(path="http_services[].routes[].action.root_directory" label="Static root directory")
      Input(v-model="action.root_directory" required placeholder="/srv/www")
      small Authenticated configuration only; the absolute root is suppressed from topology views.
    ConfigField(path="http_services[].routes[].action.spa_fallback" label="SPA fallback path")
      Input(:model-value="action.spa_fallback ?? ''" placeholder="Optional relative path" @input="setFallback")
  StringListField(
    v-model="action.index_files"
    label="Index files"
    item-label="index file"
    field-path="http_services[].routes[].action.index_files"
    hint="Safe relative filenames only; at most eight."
    :max-items="8"
    @update:model-value="emit('changed')"
  )
  .field-grid
    ConfigField(path="http_services[].routes[].action.internal_index_redirects" label="Re-evaluate routes for index files" check v-model="action.internal_index_redirects")
    ConfigField(path="http_services[].routes[].action.directory_redirects" label="Redirect directories to trailing slash" check v-model="action.directory_redirects")
    ConfigField(path="http_services[].routes[].action.etag" label="Emit ETag validators" check v-model="action.etag")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import StringListField from '../StringListField.vue'
import type { HttpStaticFilesActionConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'

const props = defineProps<{ action: HttpStaticFilesActionConfig }>()
const emit = defineEmits<{ changed: [] }>()

function setFallback(event: Event): void {
  props.action.spa_fallback = (event.target as HTMLInputElement).value || null
}
</script>
