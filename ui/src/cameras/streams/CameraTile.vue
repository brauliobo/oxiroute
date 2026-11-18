<script setup lang="ts" vapor>
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import Card from '#design/panel/Card.vue'
import StatusBadge from '#design/status/StatusBadge.vue'
import type { RtmpClientSnapshot } from '../../api'
import Facts from '../../workspace/Facts.vue'
import { cameraFacts } from './facts'
import type { Camera, RecorderView } from './model'
import PreviewPane from './PreviewPane.vue'
import RecorderRows from './RecorderRows.vue'
import ViewerList from './ViewerList.vue'
import './page.css'

const props = defineProps<{
  camera: Camera
  nowUnixMs: number
  token: string
  previewing: boolean
  canPreview: boolean
  disabled: boolean
}>()
const emit = defineEmits<{
  preview: []
  unauthorized: []
  startRecording: [recorder: RecorderView]
  stopRecording: [recorder: RecorderView]
  dropPublisher: [client: RtmpClientSnapshot]
  dropViewer: [client: RtmpClientSnapshot]
}>()

const facts = computed(() => cameraFacts(props.camera, props.nowUnixMs))
const online = computed(() => props.camera.publisher !== null)
const publisherClient = computed(() => props.camera.publisher?.client ?? null)
</script>

<template lang="pug">
Card.ox-cams-tile(:data-camera="camera.name" :data-status="camera.status")
  header.ox-cams-head
    div.ox-cams-title
      h3 {{ camera.title }}
      p.ox-muted {{ camera.service }}
    StatusBadge(:status="camera.status")

  PreviewPane(v-if="previewing && online" :camera="camera" :token="token" @unauthorized="emit('unauthorized')")
  .ox-cams-frame(v-else)
    p.ox-muted(v-if="!online") {{ camera.status === 'offline' ? 'No picture: the publisher is gone.' : 'No picture: nobody is publishing.' }}
    p.ox-muted(v-else-if="!canPreview") This browser cannot play HLS, so the preview is not available here.
    p.ox-muted(v-else) The preview starts when you ask for it, so idle tiles cost no bandwidth.

  ul.ox-cams-reasons(v-if="camera.reasons.length" aria-label="Problems")
    li(v-for="reason in camera.reasons" :key="reason")
      Badge(tone="warn" label="Problem")
      span {{ reason }}

  Facts(:items="facts")
  RecorderRows(
    v-if="camera.status !== 'offline'"
    :camera="camera"
    :disabled="disabled"
    @start="emit('startRecording', $event)"
    @stop="emit('stopRecording', $event)"
  )

  .ox-cams-actions(v-if="online")
    Button(
      :pressed="previewing"
      :disabled="!canPreview"
      :aria-label="`${previewing ? 'Stop preview of' : 'Preview'} ${camera.title}`"
      @click="emit('preview')"
    ) {{ previewing ? 'Stop preview' : 'Preview' }}
    Button(
      variant="danger"
      :disabled="disabled || !publisherClient"
      :aria-label="`Drop publisher of ${camera.title}`"
      @click="publisherClient && emit('dropPublisher', publisherClient)"
    ) Drop publisher
  p.ox-muted(v-if="online && !publisherClient") The publisher session is not in the client list yet, so it cannot be dropped from here.
  ViewerList(:camera="camera" :now-unix-ms="nowUnixMs" :disabled="disabled" @drop="emit('dropViewer', $event)")
</template>
