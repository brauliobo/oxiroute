<template lang="pug">
.action-editor
  .field-grid
    label.field.field-wide(data-field="http_services[].routes[].action.root_directory")
      span Static root directory
      input(type="text" v-model="action.root_directory" required placeholder="/srv/www")
      small Authenticated configuration only; the absolute root is suppressed from topology views.
    label.field(data-field="http_services[].routes[].action.spa_fallback")
      span SPA fallback path
      input(type="text" :value="action.spa_fallback ?? ''" placeholder="Optional relative path" @input="setFallback")
  StringListField(
    v-model="action.index_files"
    label="Index files"
    item-label="index file"
    field-path="http_services[].routes[].action.index_files"
    hint="Safe relative filenames only; at most eight."
    :max-items="8"
    @update:model-value="$emit('changed')"
  )
  .field-grid
    label.enable-row(data-field="http_services[].routes[].action.internal_index_redirects")
      input(type="checkbox" v-model="action.internal_index_redirects")
      span Re-evaluate routes for index files
    label.enable-row(data-field="http_services[].routes[].action.directory_redirects")
      input(type="checkbox" v-model="action.directory_redirects")
      span Redirect directories to trailing slash
</template>

<script setup lang="ts">
import StringListField from '../StringListField.vue'
import type { HttpStaticFilesActionConfig } from '../config'

const props = defineProps<{ action: HttpStaticFilesActionConfig }>()
defineEmits<{ changed: [] }>()

function setFallback(event: Event): void {
  props.action.spa_fallback = (event.target as HTMLInputElement).value || null
}
</script>
