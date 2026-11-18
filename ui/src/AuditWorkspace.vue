<template lang="pug">
WorkspaceSection(
  title="Audit"
  description="Authenticated, redacted records from the durable audit store. This view never substitutes the non-durable event ring."
  :busy="loading"
)
  template(v-slot:actions)
    p(role="status")
      Badge(:tone="connection.tone" :label="connection.label")
    Button(:disabled="loading || capabilityUnavailable !== null" @click="reload") {{ loading ? 'Refreshing' : 'Refresh audit' }}

  Notice(v-if="capabilityUnavailable" kind="alert")
    Banner(tone="warn")
      strong Durable audit unavailable.
      |  {{ capabilityUnavailable }} The durable route is unavailable, so browsing is disabled. The non-durable Events view is not used as a fallback.
      template(v-slot:actions)
        Button(@click="retryCapability") Retry audit capability check

  template(v-else)
    Loading(v-if="loading && records.length === 0" label="Reading durable audit history")
    Notice(v-if="pageError" kind="alert")
      Banner(tone="bad")
        strong Audit history unavailable.
        |  {{ pageError }} No event-ring fallback was attempted.
    Notice(v-if="statusError" kind="alert")
      Banner(tone="bad")
        strong Audit persistence status unavailable.
        |  {{ statusError }} Record browsing remains on the durable route only.

    StoreHealth(v-if="auditStatus" :status="auditStatus")
    EmptyState(v-else-if="!statusError && !statusLoading" title="No store status") Status has not been reported yet.

    Panel(title="Redacted records")
      .ox-stack
        AuditFilters(:applied="applied" :disabled="loading" @apply="applyFilters" @clear="clearFilters")
        p.ox-muted(role="status") {{ pageSummary }}
        EmptyState(v-if="records.length === 0 && !loading" title="No records") No durable audit records match this query.
        RecordsTable(v-else-if="records.length" :records="records")
        .ox-more(v-if="hasMore")
          Button(:disabled="loading" @click="loadNextPage") Load more durable records
</template>

<script setup lang="ts" vapor>
import { computed, ref, watch } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'

import { ApiError, fetchAudit, fetchAuditStatus } from './api'
import type { AuditPage, AuditStatus } from './api'
import AuditFilters from './audit/AuditFilters.vue'
import { connectionBadge } from './audit/model'
import type { ConnectionState } from './audit/model'
import RecordsTable from './audit/RecordsTable.vue'
import StoreHealth from './audit/StoreHealth.vue'
import type { AuditFilterSelection } from './audit/types'
import { presentApiError } from './formatters'
import { plural } from './workspace/plural'
import WorkspaceSection from './workspace/WorkspaceSection.vue'

const PAGE_SIZE = 100
const NO_FILTERS: AuditFilterSelection = { category: '', result: '' }

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const records = ref<AuditPage['records']>([])
const cursor = ref(0)
const latestCursor = ref(0)
const oldestCursor = ref<number | null>(null)
const hasMore = ref(false)
const auditStatus = ref<AuditStatus | null>(null)
const applied = ref<AuditFilterSelection>(NO_FILTERS)
const pageLoading = ref(false)
const statusLoading = ref(false)
const pageError = ref<string | null>(null)
const statusError = ref<string | null>(null)
const capabilityUnavailable = ref<string | null>(null)
let pageController: AbortController | null = null
let statusController: AbortController | null = null

const loading = computed(() => pageLoading.value || statusLoading.value)
const connectionState = computed<ConnectionState>(() => {
  if (capabilityUnavailable.value) return 'unavailable'
  return auditStatus.value?.degraded || auditStatus.value?.state === 'degraded' ? 'degraded' : 'ready'
})
const connection = computed(() => connectionBadge(connectionState.value))
const pageSummary = computed(() => {
  const oldest = oldestCursor.value === null ? '' : `, oldest retained ${oldestCursor.value}`
  return `${plural(records.value.length, 'record')} loaded. Cursor ${cursor.value}, latest ${latestCursor.value}${oldest}.`
})

async function reload(): Promise<void> {
  if (capabilityUnavailable.value) return
  await Promise.all([loadPage(0, true), loadStatus()])
}

async function loadPage(after: number, replace: boolean): Promise<void> {
  if (pageLoading.value || capabilityUnavailable.value) return
  pageController?.abort()
  const controller = new AbortController()
  pageController = controller
  pageLoading.value = true
  pageError.value = null
  try {
    const { category, result } = applied.value
    const page = await fetchAudit({
      after,
      limit: PAGE_SIZE,
      ...(category ? { category } : {}),
      ...(result ? { result } : {}),
    }, props.token, controller.signal)
    if (controller.signal.aborted) return
    applyPage(page, replace)
  } catch (error) {
    if (controller.signal.aborted) return
    if (error instanceof ApiError && error.status === 401) emit('unauthorized')
    if (isMissingRoute(error)) {
      capabilityUnavailable.value = presentApiError(error, 'The durable audit route is unavailable.')
      return
    }
    pageError.value = presentApiError(error, 'The durable audit route did not respond.')
  } finally {
    if (pageController === controller) pageController = null
    pageLoading.value = false
  }
}

async function loadStatus(): Promise<void> {
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
    statusError.value = isMissingRoute(error)
      ? 'The status route is unavailable; no non-durable status source was used.'
      : presentApiError(error, 'The audit status route did not respond.')
  } finally {
    if (statusController === controller) statusController = null
    statusLoading.value = false
  }
}

function applyPage(page: AuditPage, replace: boolean): void {
  records.value = replace ? page.records : mergeRecords(records.value, page.records)
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

function applyFilters(filters: AuditFilterSelection): void {
  applied.value = filters
  void loadPage(0, true)
}

function clearFilters(): void {
  applyFilters(NO_FILTERS)
}

const loadNextPage = (): Promise<void> => loadPage(cursor.value, false)

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
  applied.value = NO_FILTERS
  pageError.value = null
  statusError.value = null
  capabilityUnavailable.value = null
}

function isMissingRoute(error: unknown): boolean {
  return error instanceof ApiError && error.status === 404 &&
    (error.code === null || error.code === 'route_not_found')
}

watch(() => props.token, () => {
  resetWorkspace()
  void reload()
}, { immediate: true })
</script>
