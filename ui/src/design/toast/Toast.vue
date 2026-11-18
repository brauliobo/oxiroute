<script setup vapor lang="ts">
import Button from '../button/Button.vue'
import type { ToastTone } from './state'
import './styles.css'

defineProps<{ tone: ToastTone; text: string }>()
const emit = defineEmits<{ dismiss: [] }>()
</script>

<template lang="pug">
.ox-toast(:data-tone="tone" :role="tone === 'bad' ? 'alert' : 'status'")
  span {{ text }}
  Button(small aria-label="Dismiss notification" @click="emit('dismiss')") Dismiss
</template>
