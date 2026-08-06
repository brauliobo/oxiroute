<template lang="pug">
section.audit-workspace(aria-labelledby="audit-heading" :aria-busy="loading")
  header.workspace-heading
    div
      p.eyebrow Durable control history
      h2#audit-heading Audit
      p.workspace-deck Authenticated, redacted records from the durable audit store. This view never substitutes the non-durable event ring.
    .audit-actions
      span.audit-state(:class="`audit-${connectionState}`" role="status") {{ connectionStateLabel }}
      button.secondary-button(type="button" :disabled="loading || !token || capabilityUnavailable !== null" @click="reload") {{ loading ? 'Refreshing...' : 'Refresh audit' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to browse durable audit records.

  .capability-panel(v-else-if="capabilityUnavailable" role="alert")
    span.capability-index 404
    div
      h3 Durable audit unavailable
      p {{ capabilityUnavailable }}
      p The durable route is unavailable, so browsing is disabled. The non-durable Events view is not used as a fallback.
      button.secondary-button(type="button" @click="retryCapability") Retry audit capability check

  .loading-panel(v-else-if="loading && records.length === 0" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading durable audit history
      p Waiting for the authenticated cursor page and persistence status.

  template(v-if="token && capabilityUnavailable === null")
    p.notice.error-notice(v-if="pageError" role="alert")
      strong Audit history unavailable.
      |  {{ pageError }} No event-ring fallback was attempted.
    p.notice.error-notice(v-if="statusError" role="alert")
      strong Audit persistence status unavailable.
      |  {{ statusError }} Record browsing remains on the durable route only.
    p.notice.warning-notice(v-if="auditStatus?.state === 'degraded' || auditStatus?.degraded" role="status")
      strong Durable audit is degraded.
      |  Persistence reported {{ auditStatus?.lastError ?? 'a bounded storage problem' }}. Records shown below are only those returned by the durable route.
    p.notice.warning-notice(v-else-if="auditStatus?.state === 'memory' || auditStatus?.persistent === false" role="status")
      strong Durable persistence is not active.
      |  The API reports memory-only audit state; no event stream is being treated as durable history.

    section.filters-panel(aria-labelledby="audit-filters-heading")
      header.panel-heading
        div
          p.eyebrow Query controls
          h3#audit-filters-heading Filter records
        span.panel-note(v-if="selectedCategory || selectedResult") Filtered view
      form.audit-filters(@submit.prevent="applyFilters")
        label.field(for="audit-category")
          span Category
          select#audit-category(v-model="draftCategory" :disabled="loading")
            option(value="") All categories
            option(value="reload") Reload
            option(value="import") Import
            option(value="certificate") Certificate
            option(value="control") Control
        label.field(for="audit-result")
          span Result
          select#audit-result(v-model="draftResult" :disabled="loading")
            option(value="") All results
            option(value="requested") Requested
            option(value="succeeded") Succeeded
            option(value="failed") Failed
            option(value="rejected") Rejected
            option(value="conflict") Conflict
            option(value="partial") Partial
            option(value="degraded") Degraded
        .filter-buttons
          button.primary-button(type="submit" :disabled="loading") Apply filters
          button.secondary-button(type="button" :disabled="loading || (!draftCategory && !draftResult && !selectedCategory && !selectedResult)" @click="clearFilters") Clear filters

    section.status-panel(aria-labelledby="audit-status-heading")
      header.panel-heading
        div
          p.eyebrow Persistence status
          h3#audit-status-heading Store health
        span.status-chip(v-if="auditStatus" :class="`status-${auditStatus.state}`") {{ statusLabel }}
      p.empty-list(v-if="!auditStatus && !statusError && !statusLoading") Status has not been reported yet.
      .status-grid(v-if="auditStatus")
        .status-fact
          span.label Persistence
          strong {{ auditStatus.persistent ? 'Persistent' : 'Memory only' }}
        .status-fact
          span.label Records retained
          strong {{ formatCount(auditStatus.recordCount) }} / {{ formatCount(auditStatus.maxRecords) }}
        .status-fact
          span.label Store size
          strong {{ formatBytes(auditStatus.bytes) }} / {{ formatBytes(auditStatus.maxTotalBytes) }}
        .status-fact
          span.label Corrupt records
          strong {{ formatCount(auditStatus.corruptRecords) }}
        .status-fact
          span.label Write failures
          strong {{ formatCount(auditStatus.writeFailures) }}
        .status-fact
          span.label Rotated files
          strong {{ formatCount(auditStatus.rotatedFiles) }} / {{ formatCount(auditStatus.maxRotatedFiles) }}

    section.history-panel(aria-labelledby="audit-history-heading")
      header.panel-heading
        div
          p.eyebrow Cursor {{ cursor }} / latest {{ latestCursor }}
          h3#audit-history-heading Redacted records
        span.panel-note(v-if="oldestCursor !== null") Oldest retained {{ oldestCursor }}
      p.empty-list(v-if="records.length === 0 && !loading") No durable audit records match this query.
      .audit-list(v-else)
        article.audit-record(v-for="record in records" :key="record.id")
          header.record-heading
            .record-identity
              span.record-id Record {{ record.id }}
              strong {{ record.operation }}
            span.result-chip(:class="`result-${record.result}`") {{ record.result }}
          dl.record-fields
            div
              dt Timestamp
              dd {{ formatTime(record.timestampUnixMs) }}
            div
              dt Category
              dd {{ record.category }}
            div
              dt Actor
              dd {{ record.actor }}
            div
              dt Source
              dd {{ record.source }}
            div
              dt Correlation
              dd {{ record.correlationId }}
            div(v-if="record.revision")
              dt Revision
              dd {{ shortRevision(record.revision) }}
      .history-actions(v-if="hasMore")
        button.secondary-button(type="button" :disabled="loading" @click="loadNextPage") Load more durable records
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'

import {
  ApiError,
  fetchAudit,
  fetchAuditStatus,
  type AuditCategory,
  type AuditPage,
  type AuditResult,
  type AuditStatus,
} from './api'
import { formatBytes, formatCount } from './formatters'

const PAGE_SIZE = 100

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const records = ref<AuditPage['records']>([])
const cursor = ref(0)
const latestCursor = ref(0)
const oldestCursor = ref<number | null>(null)
const hasMore = ref(false)
const auditStatus = ref<AuditStatus | null>(null)
const selectedCategory = ref<AuditCategory | ''>('')
const selectedResult = ref<AuditResult | ''>('')
const draftCategory = ref<AuditCategory | ''>('')
const draftResult = ref<AuditResult | ''>('')
const pageLoading = ref(false)
const statusLoading = ref(false)
const pageError = ref<string | null>(null)
const statusError = ref<string | null>(null)
const capabilityUnavailable = ref<string | null>(null)
let pageController: AbortController | null = null
let statusController: AbortController | null = null

const loading = computed(() => pageLoading.value || statusLoading.value)
const connectionState = computed<'unauthenticated' | 'unavailable' | 'ready' | 'degraded'>(() => {
  if (!props.token) return 'unauthenticated'
  if (capabilityUnavailable.value) return 'unavailable'
  if (auditStatus.value?.degraded || auditStatus.value?.state === 'degraded') return 'degraded'
  return 'ready'
})
const connectionStateLabel = computed(() => ({
  unauthenticated: 'Token required',
  unavailable: 'Route unavailable',
  ready: 'Durable store connected',
  degraded: 'Durable store degraded',
}[connectionState.value]))
const statusLabel = computed(() => {
  if (!auditStatus.value) return ''
  return auditStatus.value.state === 'memory' ? 'memory only' : auditStatus.value.state
})

async function reload(): Promise<void> {
  if (!props.token || capabilityUnavailable.value) return
  await Promise.all([loadPage(0, true), loadStatus()])
}

async function loadPage(after: number, replace: boolean): Promise<void> {
  if (!props.token || pageLoading.value || capabilityUnavailable.value) return
  pageController?.abort()
  const controller = new AbortController()
  pageController = controller
  pageLoading.value = true
  pageError.value = null
  try {
    const page = await fetchAudit({
      after,
      limit: PAGE_SIZE,
      ...(selectedCategory.value ? { category: selectedCategory.value } : {}),
      ...(selectedResult.value ? { result: selectedResult.value } : {}),
    }, props.token, controller.signal)
    if (controller.signal.aborted) return
    applyPage(page, replace)
  } catch (error) {
    if (controller.signal.aborted) return
    if (error instanceof ApiError && error.status === 401) emit('unauthorized')
    if (isMissingRoute(error)) {
      capabilityUnavailable.value = errorMessage(error, 'The durable audit route is unavailable.')
      return
    }
    pageError.value = errorMessage(error, 'The durable audit route did not respond.')
  } finally {
    if (pageController === controller) pageController = null
    pageLoading.value = false
  }
}

async function loadStatus(): Promise<void> {
  if (!props.token) return
  statusController?.abort()
  const controller = new AbortController()
  statusController = controller
  statusLoading.value = true
  statusError.value = null
  try {
    const response = await fetchAuditStatus(props.token, controller.signal)
    if (controller.signal.aborted) return
    auditStatus.value = response.audit
  } catch (error) {
    if (controller.signal.aborted) return
    if (error instanceof ApiError && error.status === 401) emit('unauthorized')
    if (isMissingRoute(error)) {
      statusError.value = 'The status route is unavailable; no non-durable status source was used.'
    } else {
      statusError.value = errorMessage(error, 'The audit status route did not respond.')
    }
  } finally {
    if (statusController === controller) statusController = null
    statusLoading.value = false
  }
}

function applyPage(page: AuditPage, replace: boolean): void {
  const next = replace ? page.records : mergeRecords(records.value, page.records)
  records.value = next
  cursor.value = page.cursor
  latestCursor.value = page.latestCursor
  oldestCursor.value = page.oldestCursor
  hasMore.value = page.hasMore
}

function mergeRecords(current: AuditPage['records'], incoming: AuditPage['records']): AuditPage['records'] {
  const byId = new Map(current.map((record) => [record.id, record]))
  for (const record of incoming) byId.set(record.id, record)
  return [...byId.values()].sort((left, right) => left.id - right.id)
}

function applyFilters(): void {
  selectedCategory.value = draftCategory.value
  selectedResult.value = draftResult.value
  void loadPage(0, true)
}

function clearFilters(): void {
  draftCategory.value = ''
  draftResult.value = ''
  selectedCategory.value = ''
  selectedResult.value = ''
  void loadPage(0, true)
}

function loadNextPage(): void {
  void loadPage(cursor.value, false)
}

function retryCapability(): void {
  capabilityUnavailable.value = null
  void reload()
}

function resetWorkspace(): void {
  pageController?.abort()
  statusController?.abort()
  records.value = []
  cursor.value = 0
  latestCursor.value = 0
  oldestCursor.value = null
  hasMore.value = false
  auditStatus.value = null
  selectedCategory.value = ''
  selectedResult.value = ''
  draftCategory.value = ''
  draftResult.value = ''
  pageError.value = null
  statusError.value = null
  capabilityUnavailable.value = null
}

function isMissingRoute(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404 &&
    (error.code === null || error.code === 'route_not_found')
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'short',
    timeStyle: 'medium',
  }).format(timestamp)
}

function shortRevision(revision: string): string {
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

watch(() => props.token, (token) => {
  resetWorkspace()
  if (token) void reload()
}, { immediate: true })

</script>

<style scoped>
.audit-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.panel-heading,
.audit-actions,
.record-heading,
.history-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.workspace-heading {
  align-items: flex-end;
  margin-bottom: 28px;
}

.audit-actions {
  flex-wrap: wrap;
  justify-content: flex-end;
}

.eyebrow,
.label,
dt {
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
  max-width: 700px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.capability-panel,
.filters-panel,
.status-panel,
.history-panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel,
.capability-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 220px;
  padding: clamp(24px, 5vw, 48px);
}

.capability-panel {
  border-color: #806f47;
}

.auth-panel p,
.loading-panel p,
.capability-panel p {
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
  animation: audit-spin 800ms linear infinite;
}

.audit-state,
.status-chip,
.result-chip {
  padding: 5px 8px;
  border: 1px solid #536544;
  color: #c7ef94;
  font: 700 0.63rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: uppercase;
}

.audit-unavailable,
.audit-degraded,
.status-degraded,
.result-failed,
.result-rejected,
.result-conflict {
  border-color: #81483f;
  color: #ff9b88;
}

.audit-unauthenticated,
.status-memory,
.result-requested,
.result-partial,
.result-degraded {
  border-color: #806f47;
  color: #ffcf70;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
  line-height: 1.5;
}

.error-notice {
  border-color: #ff745c;
}

.warning-notice {
  border-color: #ffbf4b;
}

.filters-panel,
.status-panel,
.history-panel {
  margin-top: 18px;
}

.panel-heading {
  align-items: flex-end;
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.panel-note,
.empty-list {
  color: #8e9686;
  font-size: 0.72rem;
}

.audit-filters {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr)) auto;
  align-items: end;
  gap: 16px;
  padding: 20px;
}

.field {
  display: grid;
  gap: 8px;
  color: #b8bfb0;
  font-size: 0.72rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.field select,
.primary-button,
.secondary-button {
  min-height: 41px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font: inherit;
}

.field select {
  min-width: 0;
  padding: 9px 11px;
  border-radius: 0;
  color: #eef2e7;
  background: #0d100c;
  font: 0.78rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: none;
}

.filter-buttons {
  display: flex;
  gap: 8px;
}

.primary-button,
.secondary-button {
  padding: 10px 14px;
  font-weight: 700;
}

.primary-button {
  border-color: #b6ff51;
  color: #11150c;
  background: #b6ff51;
}

button:disabled,
select:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

button:focus-visible,
select:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 1px;
  background: #34392f;
}

.status-fact {
  display: grid;
  gap: 7px;
  min-width: 0;
  padding: 15px;
  background: #10130e;
}

.status-fact strong {
  overflow-wrap: anywhere;
  color: #dce3d4;
  font: 0.8rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.audit-list {
  display: grid;
}

.audit-record {
  padding: 20px;
  border-bottom: 1px solid #292e26;
}

.record-heading {
  align-items: flex-start;
}

.record-identity {
  display: grid;
  gap: 6px;
  min-width: 0;
}

.record-identity strong {
  overflow-wrap: anywhere;
  color: #e1e7da;
  font-size: 0.92rem;
  font-weight: 600;
}

.record-id {
  color: #b6ff51;
  font: 0.7rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.record-fields {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 1px;
  margin: 18px 0 0;
  background: #34392f;
}

.record-fields div {
  display: grid;
  gap: 7px;
  min-width: 0;
  padding: 13px;
  background: #0e110d;
}

.record-fields dd {
  margin: 0;
  overflow-wrap: anywhere;
  color: #cbd2c4;
  font: 0.72rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.history-actions {
  justify-content: flex-start;
  padding: 16px 20px;
}

@keyframes audit-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 900px) {
  .audit-filters {
    grid-template-columns: 1fr 1fr;
  }

  .filter-buttons {
    grid-column: 1 / -1;
  }

  .status-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }

  .record-fields {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .panel-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .audit-actions {
    width: 100%;
    align-items: stretch;
    flex-direction: column;
  }

  .audit-actions > * {
    width: 100%;
  }

  .auth-panel,
  .loading-panel,
  .capability-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .audit-filters,
  .status-grid,
  .record-fields {
    grid-template-columns: 1fr;
  }

  .filter-buttons {
    align-items: stretch;
    flex-direction: column;
  }

  .record-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .primary-button,
  .secondary-button,
  .field select {
    min-height: 44px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
