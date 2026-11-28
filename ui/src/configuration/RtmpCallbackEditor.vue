<template lang="pug">
ConfigGroup.object-block(:path="fieldPath" :legend="legend")
  small.field-hint Leave empty to disable a callback. Redacted URL query parameters show &lt;redacted&gt; and stay intact on save unless replaced.
  .field-grid
    ConfigField(:path="`${fieldPath}.on_connect`" label="Connect callback")
      Input(:model-value="callbacks.on_connect ?? ''" @update:model-value="callbacks.on_connect = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_disconnect`" label="Disconnect callback")
      Input(:model-value="callbacks.on_disconnect ?? ''" @update:model-value="callbacks.on_disconnect = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_publish`" label="Publish callback")
      Input(:model-value="callbacks.on_publish ?? ''" @update:model-value="callbacks.on_publish = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_publish_done`" label="Publish done callback")
      Input(:model-value="callbacks.on_publish_done ?? ''" @update:model-value="callbacks.on_publish_done = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_play`" label="Play callback")
      Input(:model-value="callbacks.on_play ?? ''" @update:model-value="callbacks.on_play = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_play_done`" label="Play done callback")
      Input(:model-value="callbacks.on_play_done ?? ''" @update:model-value="callbacks.on_play_done = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_done`" label="Done callback")
      Input(:model-value="callbacks.on_done ?? ''" @update:model-value="callbacks.on_done = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.on_update`" label="Update callback")
      Input(:model-value="callbacks.on_update ?? ''" @update:model-value="callbacks.on_update = $event || null" autocomplete="off")
    ConfigField(:path="`${fieldPath}.notify_method`" label="Callback method")
      Select(
        v-model="callbacks.notify_method"
        :options="[{ value: 'post', label: 'POST' }, { value: 'get', label: 'GET' }]"
      )
    ConfigField(:path="`${fieldPath}.timeout_ms`" label="Callback timeout (ms)")
      NumberInput(min="1" max="86400000" step="1" v-model="callbacks.timeout_ms")
    ConfigField(:path="`${fieldPath}.notify_update_timeout_ms`" label="Update timeout (ms)")
      NumberInput(min="1" max="86400000" step="1" v-model="callbacks.notify_update_timeout_ms")
    ConfigField.compact-enable(:path="`${fieldPath}.notify_update_strict`" label="Require update callback success" check v-model="callbacks.notify_update_strict")
    ConfigField.compact-enable(:path="`${fieldPath}.notify_relay_redirect`" label="Notify relay redirects" check v-model="callbacks.notify_relay_redirect")
</template>

<script setup lang="ts" vapor>
import Input from '#design/field/Input.vue'
import NumberInput from '#design/field/NumberInput.vue'
import Select from '#design/field/Select.vue'
import type { RtmpCallbackConfig } from '../config'
import ConfigField from './fields/ConfigField.vue'
import ConfigGroup from './fields/ConfigGroup.vue'

defineProps<{
  callbacks: RtmpCallbackConfig
  fieldPath: string
  legend: string
}>()
</script>
