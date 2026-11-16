<template lang="pug">
.capability-panel.canonical-unavailable-panel(role="alert")
  span.capability-index 503
  div
    h3 Canonical configuration unavailable
    p The route is available, but the persisted canonical file could not be loaded.
    dl.unavailable-revisions
      div
        dt Disk revision
        dd
          code {{ state.diskRevision ? shortRevision(state.diskRevision) : 'Unknown' }}
      div
        dt Active revision
        dd
          code {{ state.activeRevision ? shortRevision(state.activeRevision) : 'Unknown' }}
    p(v-if="state.diskRevision === null") The disk revision is uncertain; no write can be reviewed until a successful reload.
    ol.diagnostic-list(v-if="state.diagnostics.length")
      li.diagnostic(v-for="(diagnostic, index) in state.diagnostics" :key="`${diagnostic.code}-${index}`" :class="`severity-${diagnostic.severity}`")
        .diagnostic-code
          span {{ diagnostic.severity }}
          code {{ diagnostic.code }}
        strong {{ diagnostic.message }}
    button.secondary-button(type="button" @click="emit('retry')") Retry configuration load
</template>

<script setup lang="ts" vapor>
import { shortRevision } from '../../formatters'
import type { CanonicalUnavailableState } from './loadErrors'

defineProps<{ state: CanonicalUnavailableState }>()
const emit = defineEmits<{ retry: [] }>()
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./panels.css"></style>
<style scoped src="./diagnostics.css"></style>
