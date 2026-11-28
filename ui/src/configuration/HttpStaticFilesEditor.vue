<template lang="pug">
.action-editor
  .field-grid
    ConfigField.field-wide(path="http_services[].routes[].action.root_directory" label="Static root directory")
      Input(v-model="action.root_directory" required placeholder="/srv/www")
      small Authenticated configuration only; the absolute root is suppressed from topology views.
    ConfigField(path="http_services[].routes[].action.path_mapping" label="Path mapping")
      Select(
        v-model="action.path_mapping"
        :options="[{ value: 'root', label: 'Root (append full request path)' }, { value: 'alias', label: 'Alias (replace matched prefix)' }]"
      )
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
    ConfigField(path="http_services[].routes[].action.autoindex" label="Enable directory listings (autoindex)" check v-model="action.autoindex")
    ConfigField(v-if="action.autoindex" path="http_services[].routes[].action.autoindex_exact_size" label="Autoindex exact byte sizes" check v-model="action.autoindex_exact_size")
    ConfigField(v-if="action.autoindex" path="http_services[].routes[].action.autoindex_local_time" label="Autoindex local timestamps" check v-model="action.autoindex_local_time")

  ConfigGroup.route-list(path="http_services[].routes[].action.try_files")
    template(#legend)
      .cfg-route-heading
        legend Try-files candidates
        Button.add-row(@click="addTryFile") + Add candidate
    ItemScope.route-card(v-for="(entry, idx) in action.try_files" :key="idx" :index="idx")
      header.cfg-route-card-heading
        strong Candidate {{ idx + 1 }}
        Button.danger-link(variant="danger" small @click="removeTryFile(idx)") Remove
      .field-grid
        ConfigField(path="http_services[].routes[].action.try_files[].type" label="Candidate type")
          Select(
            :model-value="entry.type"
            @update:model-value="changeTryFileType(idx, $event)"
            :options="[{ value: 'request_path', label: 'Request path ($uri)' }, { value: 'request_path_directory', label: 'Request directory ($uri/)' }, { value: 'relative', label: 'Relative fallback path' }, { value: 'status', label: 'Terminal HTTP status' }]"
          )
        ConfigField(v-if="entry.type === 'relative'" path="http_services[].routes[].action.try_files[].path" label="Relative path")
          Input(v-model="entry.path" placeholder="index.html")
        ConfigField(v-if="entry.type === 'status'" path="http_services[].routes[].action.try_files[].status" label="HTTP status")
          NumberInput(min="100" max="599" step="1" v-model="entry.status")

  ConfigGroup.object-block(path="http_services[].routes[].action.mime" legend="MIME types")
    ConfigField(path="http_services[].routes[].action.mime.default_type" label="Default Content-Type")
      Input(:model-value="action.mime.default_type ?? ''" placeholder="application/octet-stream" @update:model-value="action.mime.default_type = $event || null")
    ConfigGroup.route-list(path="http_services[].routes[].action.mime.types")
      template(#legend)
        .cfg-route-heading
          legend Extension overrides
          Button.add-row(@click="addMimeType") + Add MIME type
      ItemScope.route-card(v-for="(mime, idx) in action.mime.types" :key="idx" :index="idx")
        header.cfg-route-card-heading
          strong MIME {{ idx + 1 }}
          Button.danger-link(variant="danger" small @click="removeMimeType(idx)") Remove
        .field-grid
          ConfigField(path="http_services[].routes[].action.mime.types[].extension" label="Extension")
            Input(v-model="mime.extension" placeholder="wasm")
          ConfigField(path="http_services[].routes[].action.mime.types[].content_type" label="Content-Type")
            Input(v-model="mime.content_type" placeholder="application/wasm")

  ConfigGroup.route-list(path="http_services[].routes[].action.headers")
    template(#legend)
      .cfg-route-heading
        legend Static response headers
        Button.add-row(@click="addHeader") + Add header
    ItemScope.route-card(v-for="(header, idx) in action.headers" :key="idx" :index="idx")
      header.cfg-route-card-heading
        strong Header {{ idx + 1 }}
        Button.danger-link(variant="danger" small @click="removeHeader(idx)") Remove
      .field-grid
        ConfigField(path="http_services[].routes[].action.headers[].name" label="Name")
          Input(v-model="header.name" placeholder="cache-control")
        ConfigField(path="http_services[].routes[].action.headers[].value" label="Value")
          Input(v-model="header.value" placeholder="public, max-age=3600")
        ConfigField(path="http_services[].routes[].action.headers[].always" label="Emit on error responses too" check :model-value="header.always ?? false" @update:model-value="header.always = Boolean($event)")

  ConfigGroup.route-list(path="http_services[].routes[].action.error_responses")
    template(#legend)
      .cfg-route-heading
        legend Custom error responses
        Button.add-row(@click="addErrorResponse") + Add error response
    ItemScope.route-card(v-for="(errResp, idx) in action.error_responses" :key="idx" :index="idx")
      header.cfg-route-card-heading
        strong Error response {{ idx + 1 }}
        Button.danger-link(variant="danger" small @click="removeErrorResponse(idx)") Remove
      NumberListField(
        v-model="errResp.statuses"
        label="HTTP statuses"
        item-label="status"
        field-path="http_services[].routes[].action.error_responses[].statuses"
        :default-value="404"
        :min="400"
        :max="599"
        :min-items="1"
        :max-items="32"
      )
      .field-grid
        ConfigField(path="http_services[].routes[].action.error_responses[].file" label="Relative error file")
          Input(:model-value="errResp.file ?? ''" placeholder="404.html" @update:model-value="errResp.file = $event || null")
        ConfigField(path="http_services[].routes[].action.error_responses[].body" label="Inline error body")
          Input(:model-value="errResp.body ?? ''" placeholder="Optional literal body" @update:model-value="errResp.body = $event || null")
        ConfigField(path="http_services[].routes[].action.error_responses[].internal_redirect" label="Internal redirect path")
          Input(:model-value="errResp.internal_redirect ?? ''" placeholder="Optional /errors/404" @update:model-value="errResp.internal_redirect = $event || null")
</template>

<script setup lang="ts" vapor>
import Button from '#design/button/Button.vue'
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import StringListField from '../StringListField.vue'
import type { HttpStaticFilesActionConfig } from '../config'
import NumberListField from './NumberListField.vue'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'
import ItemScope from './fields/ItemScope.vue'

const props = defineProps<{ action: HttpStaticFilesActionConfig }>()
const emit = defineEmits<{ changed: [] }>()

function setFallback(event: Event): void {
  props.action.spa_fallback = (event.target as HTMLInputElement).value || null
}

function addTryFile(): void {
  props.action.try_files.push({ type: 'request_path' })
  emit('changed')
}

function removeTryFile(index: number): void {
  props.action.try_files.splice(index, 1)
  emit('changed')
}

function changeTryFileType(index: number, type: string): void {
  props.action.try_files[index] = type === 'relative'
    ? { type: 'relative', path: 'index.html' }
    : type === 'status'
      ? { type: 'status', status: 404 }
      : type === 'request_path_directory'
        ? { type: 'request_path_directory' }
        : { type: 'request_path' }
  emit('changed')
}

function addMimeType(): void {
  props.action.mime.types.push({ extension: '', content_type: '' })
  emit('changed')
}

function removeMimeType(index: number): void {
  props.action.mime.types.splice(index, 1)
  emit('changed')
}

function addHeader(): void {
  props.action.headers.push({ name: '', value: '', always: false })
  emit('changed')
}

function removeHeader(index: number): void {
  props.action.headers.splice(index, 1)
  emit('changed')
}

function addErrorResponse(): void {
  props.action.error_responses.push({
    statuses: [404],
    file: '404.html',
    body: null,
    headers: [],
    internal_redirect: null,
  })
  emit('changed')
}

function removeErrorResponse(index: number): void {
  props.action.error_responses.splice(index, 1)
  emit('changed')
}
</script>
