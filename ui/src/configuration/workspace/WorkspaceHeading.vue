<template lang="pug">
header.cfg-workspace-heading
  div
    p.cfg-eyebrow Canonical control plane
    h2#configuration-heading Configuration workspace
    p.cfg-workspace-deck Edit typed objects, validate on the server, then review the source-format preview before writing.
  Button.secondary-button(
    v-if="canCheck"
    :disabled="loading"
    :title="loading ? 'A configuration read is already in progress.' : undefined"
    @click="emit('check')"
  ) {{ loading ? 'Checking disk revision...' : 'Check disk revision' }}
</template>

<script setup lang="ts" vapor>
import Button from '#design/button/Button.vue'
defineProps<{ canCheck: boolean; loading: boolean }>()
const emit = defineEmits<{ check: [] }>()
</script>
