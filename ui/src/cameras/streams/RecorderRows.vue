<script setup lang="ts" vapor>
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import type { Camera, RecorderView } from './model'
import './page.css'

defineProps<{ camera: Camera; disabled: boolean }>()
const emit = defineEmits<{ start: [recorder: RecorderView]; stop: [recorder: RecorderView] }>()
</script>

<template lang="pug">
ul.ox-cams-recorders(v-if="camera.recorders.length" :aria-label="`Recorders of ${camera.title}`")
  li.ox-cams-recorder(v-for="recorder in camera.recorders" :key="recorder.id" :data-recorder="recorder.name")
    .ox-cams-recorder-line
      strong {{ recorder.name }}
      span.ox-muted {{ recorder.manual ? 'manual' : 'automatic' }}
      Badge(:tone="recorder.tone" :label="recorder.label")
    p.ox-muted(v-if="recorder.failure") Stopped because {{ recorder.failure }}.
    p.ox-muted.ox-mono(v-else-if="recorder.file") {{ recorder.file }}
    Button(
      v-if="recorder.action"
      small
      :disabled="disabled"
      :aria-label="`${recorder.action === 'start' ? 'Start recording' : 'Stop recording'} ${camera.title} with ${recorder.name}`"
      @click="recorder.action === 'start' ? emit('start', recorder) : emit('stop', recorder)"
    ) {{ recorder.action === 'start' ? 'Start recording' : 'Stop recording' }}
p.ox-muted(v-else) No recorder is attached to this stream.
</template>
