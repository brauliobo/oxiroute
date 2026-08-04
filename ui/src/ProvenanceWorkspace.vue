<template lang="pug">
section.provenance-workspace(aria-labelledby="provenance-heading" :aria-busy="loading")
  header.workspace-heading
    div
      p.eyebrow Source and provenance
      h2#provenance-heading Provenance
      p.workspace-deck Show the source metadata the management API actually provides. Native import reports remain an offline CLI contract and are not fabricated here.
    button.secondary-button(type="button" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh source metadata' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to inspect canonical source metadata.

  .loading-panel(v-else-if="loading && !snapshot" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading source metadata
      p No provenance state is created until the configuration contract responds.

  p.notice.error-notice(v-if="error" role="alert")
    strong Provenance unavailable.
    |  {{ error }}

  template(v-if="token && snapshot")
    section.metadata-panel(aria-labelledby="metadata-heading")
      .section-heading
        div
          p.eyebrow Backend contract
          h3#metadata-heading Canonical source
        span.source-chip {{ snapshot.configFormat.toUpperCase() }}
      .metadata-grid
        .metadata-card
          span.label Composition
          strong {{ snapshot.compositional ? 'Compositional root' : 'Standalone root' }}
          small {{ snapshot.compositional ? 'Typed saves stay read-only.' : 'Typed saves can be reviewed in Configuration.' }}
        .metadata-card
          span.label Dependencies
          strong {{ snapshot.dependencyCount }}
          small {{ snapshot.dependencyCount === 1 ? 'native dependency' : 'native dependencies' }}
        .metadata-card
          span.label Disk revision
          code {{ shortRevision(snapshot.diskRevision) }}
          small Authoritative source revision
        .metadata-card
          span.label Candidate revision
          code {{ shortRevision(snapshot.candidateRevision) }}
          small Last server-normalized candidate
        .metadata-card
          span.label Active revision
          code {{ shortRevision(snapshot.activeRevision) }}
          small Generation currently serving traffic
    section.inventory-panel(aria-labelledby="objects-heading")
      header.panel-heading
        div
          p.eyebrow Redacted object inventory
          h3#objects-heading Canonical objects
        span.panel-note No source paths or secret values are rendered
      .object-counts
        .object-count(v-for="item in objectCounts" :key="item.label")
          strong {{ item.count }}
          span {{ item.label }}
    section.diagnostics-panel(aria-labelledby="provenance-diagnostics-heading")
      header.panel-heading
        div
          p.eyebrow Source diagnostics
          h3#provenance-diagnostics-heading Diagnostics
        span.panel-note {{ snapshot.diagnostics.length }}
      p.empty-list(v-if="snapshot.diagnostics.length === 0") No source diagnostics were returned.
      ol.diagnostic-list(v-else)
        li.diagnostic(v-for="(diagnostic, index) in snapshot.diagnostics" :key="`${diagnostic.code}-${index}`" :class="`severity-${diagnostic.severity}`")
          code {{ diagnostic.code }}
          div
            strong {{ diagnostic.message }}
            small {{ diagnostic.stage }}{{ diagnostic.path ? ` / ${diagnostic.path}` : '' }}
    aside.boundary-note(role="note")
      strong Native import report unavailable over management API.
      p The backend exposes import reports through the offline CLI only. This view does not call an unsupported import route, and it does not claim conversion coverage or provenance that the API did not return.
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import { ApiError, fetchConfig } from './api'
import type { ConfigSnapshot } from './config'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const snapshot = ref<ConfigSnapshot | null>(null)
const loading = ref(false)
const error = ref<string | null>(null)
let controller: AbortController | null = null

const objectCounts = computed(() => {
  const config = snapshot.value?.config
  return [
    { label: 'Certificates', count: config?.certificates.length ?? 0 },
    { label: 'TLS profiles', count: config?.tls_profiles.length ?? 0 },
    { label: 'Listeners', count: config?.listeners.length ?? 0 },
    { label: 'Pools', count: config?.upstream_pools.length ?? 0 },
    { label: 'HTTP services', count: config?.http_services.length ?? 0 },
    { label: 'Forward proxy services', count: config?.forward_proxy_services.length ?? 0 },
    { label: 'RTMP services', count: config?.rtmp_services.length ?? 0 },
    { label: 'L4 services', count: config?.l4_services.length ?? 0 },
  ]
})

async function load(): Promise<void> {
  if (!props.token || loading.value) return
  controller?.abort()
  const nextController = new AbortController()
  controller = nextController
  loading.value = true
  error.value = null
  try {
    const next = await fetchConfig(props.token, nextController.signal)
    if (nextController.signal.aborted) return
    snapshot.value = next
  } catch (requestError) {
    if (nextController.signal.aborted) return
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    error.value = requestError instanceof Error ? requestError.message : 'The configuration route did not respond.'
  } finally {
    if (controller === nextController) controller = null
    loading.value = false
  }
}

function shortRevision(revision: string | null): string {
  if (!revision) return 'None'
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

watch(() => props.token, (token) => {
  controller?.abort()
  if (!token) {
    snapshot.value = null
    error.value = null
    return
  }
  void load()
})

onMounted(() => {
  if (props.token) void load()
})

onBeforeUnmount(() => {
  controller?.abort()
})
</script>

<style scoped>
.provenance-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.section-heading,
.panel-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.workspace-heading,
.section-heading {
  align-items: flex-end;
}

.workspace-heading {
  margin-bottom: 28px;
}

.eyebrow,
.label {
  margin: 0;
  color: #929a88;
  font: 700 0.66rem/1.2 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
p {
  margin-top: 0;
}

h2,
h3 {
  font-family: Georgia, "Times New Roman", serif;
  font-weight: 400;
  letter-spacing: -0.04em;
}

h2 {
  margin: 4px 0 0;
  font-size: clamp(2.2rem, 5vw, 4rem);
}

h3 {
  margin: 4px 0 0;
  font-size: clamp(1.35rem, 3vw, 2.1rem);
}

.workspace-deck {
  max-width: 720px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.metadata-panel,
.inventory-panel,
.diagnostics-panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 220px;
  padding: clamp(24px, 5vw, 48px);
}

.auth-panel p,
.loading-panel p {
  margin: 7px 0 0;
  color: #8e9686;
}

.capability-index {
  color: #ffbf4b;
  font: clamp(3.5rem, 9vw, 7rem) Georgia, "Times New Roman", serif;
}

.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: provenance-spin 800ms linear infinite;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid #ff745c;
  background: #171a15;
  color: #cdd2c5;
}

.metadata-panel,
.inventory-panel,
.diagnostics-panel {
  margin-top: 22px;
}

.metadata-panel {
  padding: 22px;
}

.source-chip,
.panel-note,
.empty-list,
.boundary-note p,
.diagnostic small {
  color: #8e9686;
  font-size: 0.72rem;
}

.source-chip {
  padding: 5px 8px;
  border: 1px solid #5b7269;
  color: #a7ded0;
  font: 700 0.63rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.metadata-grid,
.object-counts {
  display: grid;
  gap: 1px;
  margin-top: 18px;
  background: #34392f;
}

.metadata-grid {
  grid-template-columns: repeat(5, minmax(0, 1fr));
}

.metadata-card,
.object-count {
  display: grid;
  gap: 8px;
  min-width: 0;
  padding: 15px;
  background: #0e110d;
}

.metadata-card strong,
.metadata-card code {
  overflow-wrap: anywhere;
  color: #dce3d4;
  font: 0.78rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.metadata-card small {
  color: #777f70;
  font-size: 0.7rem;
  line-height: 1.4;
}

.panel-heading {
  align-items: flex-end;
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.object-counts {
  grid-template-columns: repeat(4, minmax(0, 1fr));
  margin: 0;
}

.object-count strong {
  color: #b6ff51;
  font: 1.45rem Georgia, "Times New Roman", serif;
}

.object-count span {
  color: #8e9686;
  font-size: 0.7rem;
}

.empty-list {
  margin: 0;
  padding: 22px 20px;
}

.diagnostic-list {
  margin: 0;
  padding: 0;
  list-style: none;
}

.diagnostic {
  display: grid;
  grid-template-columns: 160px minmax(0, 1fr);
  gap: 18px;
  padding: 17px 20px;
  border-bottom: 1px solid #292e26;
  border-left: 3px solid #ffbf4b;
  background: #10130e;
}

.diagnostic.severity-error {
  border-left-color: #ff745c;
}

.diagnostic code {
  color: #dce3d4;
  font: 0.72rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.diagnostic div {
  display: grid;
  gap: 6px;
}

.diagnostic small {
  line-height: 1.4;
}

.boundary-note {
  display: block;
  margin-top: 22px;
  padding: 16px 18px;
  border-left: 3px solid #806f47;
  background: #1c1c15;
  color: #cdd2c5;
}

.boundary-note p {
  margin: 7px 0 0;
  line-height: 1.55;
}

.secondary-button {
  min-height: 41px;
  padding: 10px 14px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font-weight: 700;
}

button:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

button:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

@keyframes provenance-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 1000px) {
  .metadata-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .section-heading,
  .panel-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .metadata-grid,
  .object-counts {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .auth-panel,
  .loading-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .diagnostic {
    grid-template-columns: 1fr;
    gap: 9px;
  }
}

@media (max-width: 420px) {
  .metadata-grid,
  .object-counts {
    grid-template-columns: 1fr;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
