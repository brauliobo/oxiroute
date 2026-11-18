<template lang="pug">
ConfigGroup.object-block(:path="fieldPath" :legend="legend")
  .field-grid
    ConfigField(:path="`${fieldPath}.on_connect`" label="Connect callback")
      input(type="text" v-model="callbacks.on_connect" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_disconnect`" label="Disconnect callback")
      input(type="text" v-model="callbacks.on_disconnect" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_publish`" label="Publish callback")
      input(type="text" v-model="callbacks.on_publish" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_publish_done`" label="Publish done callback")
      input(type="text" v-model="callbacks.on_publish_done" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_play`" label="Play callback")
      input(type="text" v-model="callbacks.on_play" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_play_done`" label="Play done callback")
      input(type="text" v-model="callbacks.on_play_done" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_done`" label="Done callback")
      input(type="text" v-model="callbacks.on_done" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_update`" label="Update callback")
      input(type="text" v-model="callbacks.on_update" autocomplete="off")
    ConfigField(:path="`${fieldPath}.notify_method`" label="Callback method")
      select(v-model="callbacks.notify_method")
        option(value="post") POST
        option(value="get") GET
    ConfigField(:path="`${fieldPath}.timeout_ms`" label="Callback timeout (ms)")
      input(type="number" min="1" max="86400000" step="1" v-model.number="callbacks.timeout_ms")
    ConfigField(:path="`${fieldPath}.notify_update_timeout_ms`" label="Update timeout (ms)")
      input(type="number" min="1" max="86400000" step="1" v-model.number="callbacks.notify_update_timeout_ms")
    ConfigField.compact-enable(:path="`${fieldPath}.notify_update_strict`" label="Require update callback success" check)
      input(type="checkbox" v-model="callbacks.notify_update_strict")
    ConfigField.compact-enable(:path="`${fieldPath}.notify_relay_redirect`" label="Notify relay redirects" check)
      input(type="checkbox" v-model="callbacks.notify_relay_redirect")
</template>

<script setup lang="ts" vapor>
import type { RtmpCallbackConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

defineProps<{
  callbacks: RtmpCallbackConfig
  fieldPath: string
  legend: string
}>()
</script>
