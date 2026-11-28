<template lang="pug">
Panel(title="Configuration revision history")
  .ox-ops-buttons
    Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing...' : 'Refresh history' }}
  Notice(kind="alert")
    Banner(v-if="error" tone="bad")
      strong History unavailable.
      |  {{ error }}
  Notice(kind="status")
    Banner(v-if="notice" tone="ok")
      strong {{ notice }}
  Loading(v-if="loading && !history" label="Loading configuration history")
  template(v-if="history")
    p.ox-muted
      | Format: {{ formatLabel(history.format) }} · Current revision {{ shortRevision(history.diskRevision) }} · Retaining up to {{ history.limits.maxEntries }} entries.
    EmptyState(v-if="history.entries.length === 0" title="No saved revisions yet") Saved versions of the root configuration file appear here after edits are persisted.
    Table(v-else caption="Saved root configuration revisions" :columns="COLUMNS")
      tbody
        tr(v-for="entry in history.entries" :key="entry.id")
          th.ox-name(scope="row" data-label="Revision")
            code {{ entry.id.slice(0, 12) }}
            Badge(v-if="entry.current" tone="ok" label="Current")
          td(data-label="Saved at") {{ formatTime(entry.at) }}
          td(data-label="Size") {{ entry.bytes }} B
          td(data-label="Actions")
            .ox-ops-buttons
              Button(small :disabled="diffLoading" @click="inspectDiff(entry.id)") Diff vs current
              Button(v-if="!entry.current" small variant="danger" :disabled="rollingBack" @click="confirmRollback(entry.id)") Roll back
  Panel.cfg-preview-panel(v-if="diffResult" :title="`Unified diff (${diffTargetId.slice(0, 12)} → current)`")
    p.ox-muted(v-if="diffResult.identical") This revision is byte-identical to the current disk file.
    p.ox-muted(v-else-if="diffResult.unresolved") One of the compared revisions could not be parsed.
    pre(v-else-if="diffResult.textDiff") {{ diffResult.textDiff }}
    p.ox-muted(v-else) Text diff withheld because inline secrets cannot be safely redacted.
</template>

<script setup lang="ts" vapor>
import { onMounted, ref, watch } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import {
  ApiError,
  fetchConfigHistory,
  fetchConfigHistoryDiff,
  rollbackConfigHistory,
  type ConfigHistoryDiffResponse,
  type ConfigHistoryListResponse,
} from '../../api'
import { formatLabel } from '../../config'
import { shortRevision } from '../../formatters'
import { confirmAction } from '../../shell/confirm'

const COLUMNS = ['Revision', 'Saved at', 'Size', 'Actions']
const props = defineProps<{ token: string }>()
const emit = defineEmits<{ rolledBack: []; unauthorized: [] }>()

const history = ref<ConfigHistoryListResponse | null>(null)
const loading = ref(false)
const diffLoading = ref(false)
const rollingBack = ref(false)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)
const diffTargetId = ref('')
const diffResult = ref<ConfigHistoryDiffResponse | null>(null)

function handleError(err: unknown): void {
  if (err instanceof ApiError && err.status === 401) {
    emit('unauthorized')
    return
  }
  error.value = err instanceof Error ? err.message : 'Unexpected history error'
}

function formatTime(unixMs: number): string {
  return new Date(unixMs).toLocaleString()
}

async function load(): Promise<void> {
  if (!props.token) return
  loading.value = true
  error.value = null
  try {
    history.value = await fetchConfigHistory(props.token)
  } catch (err) {
    handleError(err)
  } finally {
    loading.value = false
  }
}

async function inspectDiff(id: string): Promise<void> {
  if (!props.token) return
  diffLoading.value = true
  diffTargetId.value = id
  error.value = null
  try {
    diffResult.value = await fetchConfigHistoryDiff(id, 'current', props.token)
  } catch (err) {
    handleError(err)
  } finally {
    diffLoading.value = false
  }
}

async function confirmRollback(id: string): Promise<void> {
  if (!props.token || !history.value) return
  const ok = await confirmAction({
    title: `Roll back configuration to ${id.slice(0, 12)}?`,
    message: 'This writes the selected historical file back to disk as the new candidate revision.',
    confirmLabel: 'Roll back file',
    danger: true,
  })
  if (!ok) return
  rollingBack.value = true
  error.value = null
  notice.value = null
  try {
    const res = await rollbackConfigHistory(id, history.value.diskRevision, props.token)
    notice.value = `Rolled back disk configuration to ${id.slice(0, 12)} (new disk revision ${shortRevision(res.diskRevision)}).`
    diffResult.value = null
    await load()
    emit('rolledBack')
  } catch (err) {
    handleError(err)
  } finally {
    rollingBack.value = false
  }
}

watch(() => props.token, (token) => {
  if (token) void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>
