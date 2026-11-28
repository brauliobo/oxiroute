<template lang="pug">
Banner.revision-banner.candidate-waiting(v-if="candidateWaiting" tone="info" role="status")
  strong A saved configuration is waiting to be activated.
  |  The file on disk renders as generation {{ shortRevision(snapshot.candidateRevision) }}; generation {{ shortRevision(activeRevision ?? '') }} is serving traffic. The watcher activates the file without a restart unless the change needs one.
  template(#actions)
    Button(small variant="primary" :disabled="reloading" @click="emit('reload-now')") {{ reloading ? 'Reloading...' : 'Reload now' }}

Banner.revision-banner.compositional(v-if="snapshot.compositional" tone="info" role="status")
  strong Compositional root is read-only in structured mode — use Source &amp; includes to edit.
  |  Typed saves are unavailable because this {{ formatLabel(snapshot.configFormat) }} root resolves {{ snapshot.dependencyCount }} {{ snapshot.dependencyCount === 1 ? 'dependency' : 'dependencies' }}. Edit the root or any included file safely in the Source &amp; includes tab without flattening its source files.
  template(#actions)
    Button(small variant="primary" @click="emit('open-source')") Open in Source &amp; includes editor

Banner.revision-banner.stale(v-if="staleRevision" tone="bad" tabindex="-1" role="alert")
  strong The file on disk changed while you were editing.
  |  Your draft is kept and nothing was written. It started from {{ shortRevision(diskRevision) }}; the disk now has {{ currentRevision }}. Reload and reapply carries over your edits to settings the new file left alone and lists any that collide; discarding drops all of them.
  template(#actions)
    Button(variant="primary" @click="emit('reapply')") Reload and reapply my edits
    Button(@click="emit('discard')") Discard my edits and load disk

Banner.revision-banner.error(v-if="refreshError" tone="bad" role="alert")
  strong Configuration refresh failed.
  |  {{ refreshError }} The current draft and revision state were retained.

Banner.revision-banner.error(v-if="canonicalUnavailable" tone="bad" role="alert")
  strong Persisted canonical configuration unavailable.
  |  Disk revision {{ canonicalUnavailable.diskRevision ? shortRevision(canonicalUnavailable.diskRevision) : 'unknown' }}; active revision {{ canonicalUnavailable.activeRevision ? shortRevision(canonicalUnavailable.activeRevision) : 'unknown' }}.
  template(v-for="(diagnostic, index) in canonicalUnavailable.diagnostics" :key="`${diagnostic.code}-${index}`")
    |
    code {{ diagnostic.code }}
    |  {{ diagnostic.message }}

Banner.revision-banner.save-state(v-if="saveMessage" :class="saveMessage.kind" :tone="TONES[saveMessage.kind]" tabindex="-1" role="status")
  strong {{ saveMessage.title }}
  |  {{ saveMessage.detail }}

Banner.revision-banner.tls-alpn(v-if="tlsAlpnMessage" :class="tlsAlpnMessage.kind" :tone="TONES[tlsAlpnMessage.kind]" :role="tlsAlpnMessage.kind === 'error' ? 'alert' : 'status'")
  strong {{ tlsAlpnMessage.title }}
  |  {{ tlsAlpnMessage.detail }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import Banner from '#design/banner/Banner.vue'
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
  reloading?: boolean
}>()
const emit = defineEmits<{ discard: []; reapply: []; 'open-source': []; 'reload-now': [] }>()

const TONES = { success: 'ok', pending: 'info', error: 'bad' } as const
const candidateWaiting = computed(() =>
  props.activeRevision !== null && props.staleRevision === null && props.saveMessage === null &&
    props.snapshot.candidateRevision !== props.activeRevision)
const currentRevision = computed(() =>
  props.staleRevision === 'unknown' || props.staleRevision === 'newer revision'
    ? 'a revision that could not be read'
    : shortRevision(props.staleRevision ?? ''))
</script>
