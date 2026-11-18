<template lang="pug">
.revision-banner.diverged(v-if="diskRevision !== activeRevision" :inert="reviewOpen || undefined" role="status" aria-live="polite")
  strong Disk and active revisions differ.
  |  The saved file is not the generation currently serving traffic.

.revision-banner.compositional(v-if="snapshot.compositional" :inert="reviewOpen || undefined" role="status")
  div
    strong Compositional root is read-only.
    p Typed saves are unavailable because this {{ formatLabel(snapshot.configFormat) }} root resolves {{ snapshot.dependencyCount }} {{ snapshot.dependencyCount === 1 ? 'dependency' : 'dependencies' }}. Inspection and server validation remain available without flattening its source files.

.revision-banner.stale(v-if="staleRevision" :inert="reviewOpen || undefined" tabindex="-1" role="alert")
  div
    strong Draft preserved: the disk revision changed.
    p Expected {{ shortRevision(diskRevision) }}, current {{ shortRevision(staleRevision) }}. Reconcile explicitly; this editor will not overwrite the newer file.
  button.secondary-button(type="button" @click="emit('discard')") Discard draft and load disk

.revision-banner.error(v-if="refreshError" :inert="reviewOpen || undefined" role="alert")
  strong Configuration refresh failed.
  |  {{ refreshError }} The current draft and revision state were retained.

.revision-banner.error(v-if="canonicalUnavailable" :inert="reviewOpen || undefined" role="alert")
  strong Persisted canonical configuration unavailable.
  |  Disk revision {{ canonicalUnavailable.diskRevision ? shortRevision(canonicalUnavailable.diskRevision) : 'unknown' }}; active revision {{ canonicalUnavailable.activeRevision ? shortRevision(canonicalUnavailable.activeRevision) : 'unknown' }}.
  ol.diagnostic-list(v-if="canonicalUnavailable.diagnostics.length")
    li.diagnostic(v-for="(diagnostic, index) in canonicalUnavailable.diagnostics" :key="`${diagnostic.code}-${index}`")
      code {{ diagnostic.code }}
      span {{ diagnostic.message }}

.revision-banner.save-state(v-if="saveMessage" :inert="reviewOpen || undefined" :class="saveMessage.kind" tabindex="-1" role="status" aria-live="polite")
  strong {{ saveMessage.title }}
  |  {{ saveMessage.detail }}

.revision-banner.tls-alpn(v-if="tlsAlpnMessage" :inert="reviewOpen || undefined" :class="tlsAlpnMessage.kind" :role="tlsAlpnMessage.kind === 'error' ? 'alert' : 'status'" aria-live="polite")
  strong {{ tlsAlpnMessage.title }}
  |  {{ tlsAlpnMessage.detail }}
</template>

<script setup lang="ts" vapor>
import type { ConfigSnapshot } from '../../config'
import { shortRevision } from '../../formatters'
import type { SaveMessage } from '../useConfigurationLifecycle'
import { formatLabel } from './configFormat'
import type { CanonicalUnavailableState } from './loadErrors'
import type { TlsAlpnMessage } from './useTlsAlpnPreparation'

defineProps<{
  snapshot: ConfigSnapshot
  diskRevision: string
  activeRevision: string | null
  staleRevision: string | null
  refreshError: string | null
  canonicalUnavailable: CanonicalUnavailableState | null
  saveMessage: SaveMessage | null
  tlsAlpnMessage: TlsAlpnMessage | null
  reviewOpen: boolean
}>()
const emit = defineEmits<{ discard: [] }>()
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
