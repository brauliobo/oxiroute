<template lang="pug">
.revision-banner.candidate-waiting(v-if="candidateWaiting" role="status" aria-live="polite")
  div
    strong A saved configuration is waiting to be activated.
    p The file on disk renders as generation {{ shortRevision(snapshot.candidateRevision) }}; generation {{ shortRevision(activeRevision ?? '') }} is serving traffic. The watcher activates the file without a restart unless the change needs one.

.revision-banner.compositional(v-if="snapshot.compositional" role="status")
  div
    strong Compositional root is read-only.
    p Typed saves are unavailable because this {{ formatLabel(snapshot.configFormat) }} root resolves {{ snapshot.dependencyCount }} {{ snapshot.dependencyCount === 1 ? 'dependency' : 'dependencies' }}. Inspection and server validation remain available without flattening its source files.

.revision-banner.stale(v-if="staleRevision" tabindex="-1" role="alert")
  div
    strong The file on disk changed while you were editing.
    p Your draft is kept and nothing was written. It started from {{ shortRevision(diskRevision) }}; the disk now has {{ currentRevision }}. Reload and reapply carries over your edits to settings the new file left alone and lists any that collide; discarding drops all of them.
  .revision-actions
    Button(variant="primary" @click="emit('reapply')") Reload and reapply my edits
    Button(@click="emit('discard')") Discard my edits and load disk

.revision-banner.error(v-if="refreshError" role="alert")
  strong Configuration refresh failed.
  |  {{ refreshError }} The current draft and revision state were retained.

.revision-banner.error(v-if="canonicalUnavailable" role="alert")
  strong Persisted canonical configuration unavailable.
  |  Disk revision {{ canonicalUnavailable.diskRevision ? shortRevision(canonicalUnavailable.diskRevision) : 'unknown' }}; active revision {{ canonicalUnavailable.activeRevision ? shortRevision(canonicalUnavailable.activeRevision) : 'unknown' }}.
  ol.diagnostic-list(v-if="canonicalUnavailable.diagnostics.length")
    li.diagnostic(v-for="(diagnostic, index) in canonicalUnavailable.diagnostics" :key="`${diagnostic.code}-${index}`")
      code {{ diagnostic.code }}
      span {{ diagnostic.message }}

.revision-banner.save-state(v-if="saveMessage" :class="saveMessage.kind" tabindex="-1" role="status" aria-live="polite")
  strong {{ saveMessage.title }}
  |  {{ saveMessage.detail }}

.revision-banner.tls-alpn(v-if="tlsAlpnMessage" :class="tlsAlpnMessage.kind" :role="tlsAlpnMessage.kind === 'error' ? 'alert' : 'status'" aria-live="polite")
  strong {{ tlsAlpnMessage.title }}
  |  {{ tlsAlpnMessage.detail }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import Button from '#design/button/Button.vue'
import type { ConfigSnapshot } from '../../config'
import { shortRevision } from '../../formatters'
import type { SaveMessage } from '../useConfigurationLifecycle'
import { formatLabel } from './configFormat'
import type { CanonicalUnavailableState } from './loadErrors'
import type { TlsAlpnMessage } from './useTlsAlpnPreparation'
import './reapply.css'

const props = defineProps<{
  snapshot: ConfigSnapshot
  diskRevision: string
  activeRevision: string | null
  staleRevision: string | null
  refreshError: string | null
  canonicalUnavailable: CanonicalUnavailableState | null
  saveMessage: SaveMessage | null
  tlsAlpnMessage: TlsAlpnMessage | null
}>()
const emit = defineEmits<{ discard: []; reapply: [] }>()
const candidateWaiting = computed(() =>
  props.activeRevision !== null && props.staleRevision === null && props.saveMessage === null &&
    props.snapshot.candidateRevision !== props.activeRevision)
const currentRevision = computed(() =>
  props.staleRevision === 'unknown' || props.staleRevision === 'newer revision'
    ? 'a revision that could not be read'
    : shortRevision(props.staleRevision ?? ''))
</script>

<style scoped src="./base.css"></style>
<style scoped src="./controls.css"></style>
<style scoped src="./diagnostics.css"></style>
<style scoped>
.revision-banner {
  align-items: flex-start;
  margin-top: 10px;
  padding: 13px 16px;
  border-left: 3px solid #ffbf4b;
  color: #cdd2c5;
  background: #191a14;
}

.revision-banner p {
  margin: 5px 0 0;
}

.revision-banner.error,
.revision-banner.stale {
  border-color: #ff745c;
}

.revision-banner.success {
  border-color: #b6ff51;
}
</style>
