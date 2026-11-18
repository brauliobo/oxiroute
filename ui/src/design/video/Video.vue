<script setup vapor lang="ts">
import { onMounted, useTemplateRef } from 'vue'
import './styles.css'

defineProps<{ src: string; label: string }>()
const emit = defineEmits<{ playing: []; failed: [] }>()
const video = useTemplateRef<HTMLVideoElement>('video')

onMounted(() => {
  if (video.value) video.value.muted = true
})
</script>

<template lang="pug">
video.ox-video(
  ref="video"
  :src="src"
  :aria-label="label"
  controls
  autoplay
  playsinline
  @playing="emit('playing')"
  @error="emit('failed')"
)
</template>
