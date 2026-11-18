<template lang="pug">
header.workspace-heading(:inert="reviewOpen || undefined")
  div
    p.eyebrow Canonical control plane
    h2#configuration-heading Configuration workspace
    p.workspace-deck Edit typed objects, validate on the server, then review the source-format preview before writing.
  button.secondary-button(
    v-if="canCheck"
    type="button"
    :disabled="loading"
    :title="loading ? 'A configuration read is already in progress.' : undefined"
    @click="emit('check')"
  ) {{ loading ? 'Checking disk revision...' : 'Check disk revision' }}
</template>

<script setup lang="ts" vapor>
defineProps<{ canCheck: boolean; loading: boolean; reviewOpen: boolean }>()
const emit = defineEmits<{ check: [] }>()
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped>
.workspace-deck {
  max-width: 680px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}
</style>
