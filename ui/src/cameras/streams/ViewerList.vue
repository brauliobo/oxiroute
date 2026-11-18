<script setup lang="ts" vapor>
import { ref } from 'vue'
import Button from '#design/button/Button.vue'
import Toggle from '#design/button/Toggle.vue'
import { formatAge } from '#design/format'
import type { RtmpClientSnapshot } from '../../api'
import { pluralize } from '../../formatters'
import type { Camera } from './model'
import './page.css'

const props = defineProps<{ camera: Camera; nowUnixMs: number; disabled: boolean }>()
const emit = defineEmits<{ drop: [client: RtmpClientSnapshot] }>()

const open = ref(false)
const age = (client: RtmpClientSnapshot) => formatAge(Math.max(0, props.nowUnixMs - client.connectedAtUnixMs))
</script>

<template lang="pug">
.ox-cams-viewers(v-if="camera.viewers > 0")
  Toggle(:expanded="open" @click="open = !open") {{ pluralize(camera.viewers, 'viewer') }}
  ul.ox-cams-viewer-list(v-if="open" :aria-label="`Viewers of ${camera.title}`")
    li(v-for="client in camera.viewerClients" :key="client.id")
      span {{ client.peerIp ?? 'Unknown address' }}
      span.ox-muted connected {{ age(client) }}
      Button(
        small
        variant="danger"
        :disabled="disabled"
        :aria-label="`Drop viewer ${client.peerIp ?? client.id} of ${camera.title}`"
        @click="emit('drop', client)"
      ) Drop viewer
    li.ox-muted(v-if="camera.viewerClients.length < camera.viewers") Viewers that are not listed are not available for control yet.
</template>
