<script setup vapor lang="ts">
import { computed } from 'vue'
import Button from '#design/button/Button.vue'
import { formatTimestamp } from '#design/format'

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
  Button(variant="link" @click="emit('help')") Keyboard shortcuts (?)
</template>
