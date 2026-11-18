<script setup lang="ts" vapor>
import { onMounted } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Loading from '#design/empty/Loading.vue'
import Video from '#design/video/Video.vue'
import type { Camera } from './model'
import { usePreview } from './usePreview'
import './page.css'

const props = defineProps<{ camera: Camera; token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const { state, start, retry, playbackFailed } = usePreview(() => props.camera, () => props.token, () => emit('unauthorized'))

onMounted(start)
</script>

<template lang="pug">
.ox-cams-preview(data-preview)
  Video(v-if="state.kind === 'playing'" :src="state.src" :label="`Live preview of ${camera.title}`" @failed="playbackFailed")
  Loading(v-else-if="state.kind === 'starting'" label="Requesting the preview")
  Notice(v-else kind="status")
    Banner(:tone="state.kind === 'failed' ? 'warn' : 'info'")
      | {{ state.message }}
      template(v-slot:actions)
        Button(v-if="state.kind === 'failed'" small @click="retry") Try again
</template>
