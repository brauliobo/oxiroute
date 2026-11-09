<script setup vapor lang="ts">
import { computed } from 'vue'
import { formatTimestamp } from '../format'

const props = defineProps<{ version?: string; generation?: number; sampledAtUnixMs?: number }>()
const emit = defineEmits<{ help: [] }>()

const summary = computed(() => [
  props.version && `OxiRoute ${props.version}`,
  props.generation !== undefined && `generation ${props.generation}`,
  props.sampledAtUnixMs !== undefined && `sampled ${formatTimestamp(props.sampledAtUnixMs)}`,
].filter(Boolean).join(' · '))
</script>

<template lang="pug">
footer.ox-footer
  p {{ summary }}
  button.ox-link(type="button" @click="emit('help')") Keyboard shortcuts (?)
</template>
