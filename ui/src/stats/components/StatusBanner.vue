<script setup vapor lang="ts">
import type { Banner } from '../banner'

defineProps<{ banner: Banner | undefined }>()
const emit = defineEmits<{ action: [action: 'retry' | 'reload'] }>()
const LABELS = { retry: 'Retry now', reload: 'Reload page' }
</script>

<template lang="pug">
div(role="alert" aria-atomic="true")
  p.ox-banner(v-if="banner" :data-tone="banner.tone")
    span {{ banner.text }}
    button.ox-btn(v-if="banner.action" type="button" @click="emit('action', banner.action)") {{ LABELS[banner.action] }}
</template>
