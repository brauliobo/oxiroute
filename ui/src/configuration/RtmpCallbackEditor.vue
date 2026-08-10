<template lang="pug">
fieldset.object-block(:data-field="fieldPath")
  legend {{ legend }}
  .field-grid
    label.field(:data-field="`${fieldPath}.on_connect`")
      span Connect callback
      input(type="text" v-model="callbacks.on_connect" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_disconnect`")
      span Disconnect callback
      input(type="text" v-model="callbacks.on_disconnect" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_publish`")
      span Publish callback
      input(type="text" v-model="callbacks.on_publish" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_publish_done`")
      span Publish done callback
      input(type="text" v-model="callbacks.on_publish_done" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_play`")
      span Play callback
      input(type="text" v-model="callbacks.on_play" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_play_done`")
      span Play done callback
      input(type="text" v-model="callbacks.on_play_done" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_done`")
      span Done callback
      input(type="text" v-model="callbacks.on_done" autocomplete="off")
    label.field(:data-field="`${fieldPath}.on_update`")
      span Update callback
      input(type="text" v-model="callbacks.on_update" autocomplete="off")
    label.field(:data-field="`${fieldPath}.notify_method`")
      span Callback method
      select(v-model="callbacks.notify_method")
        option(value="post") POST
        option(value="get") GET
    label.field(:data-field="`${fieldPath}.timeout_ms`")
      span Callback timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="callbacks.timeout_ms")
    label.field(:data-field="`${fieldPath}.notify_update_timeout_ms`")
      span Update timeout (ms)
      input(type="number" min="1" max="86400000" step="1" v-model.number="callbacks.notify_update_timeout_ms")
    label.enable-row.compact-enable(:data-field="`${fieldPath}.notify_update_strict`")
      input(type="checkbox" v-model="callbacks.notify_update_strict")
      span Require update callback success
    label.enable-row.compact-enable(:data-field="`${fieldPath}.notify_relay_redirect`")
      input(type="checkbox" v-model="callbacks.notify_relay_redirect")
      span Notify relay redirects
</template>

<script setup lang="ts">
import type { RtmpCallbackConfig } from '../config'

defineProps<{
  callbacks: RtmpCallbackConfig
  fieldPath: string
  legend: string
}>()
</script>
