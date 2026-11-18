<template lang="pug">
WorkspaceSection(
  title="Provenance"
  description="Bounded, redacted evidence retained by the native import resolver. Source files stay read-only and canonical configuration is the only editable surface."
  :busy="loading"
)
  template(v-slot:actions)
    Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing' : 'Refresh reports' }}

  Loading(v-if="loading && !report" label="Reading import reports")

  Notice(v-if="error" kind="alert")
    Banner(tone="bad")
      strong Provenance unavailable.
      |  {{ error }}

  template(v-if="!loading || report")
    Panel(v-if="reports.length" title="Select a native source")
      .ox-stack
        p.ox-muted {{ plural(reports.length, 'report') }} retained
        Field(label="Native import report" label-for="import-report-selection")
          Select#import-report-selection(:model-value="String(selectedIndex)" :options="options" :disabled="loading" @update:modelValue="selectReport")
    EmptyState(v-else-if="!loading" title="No import reports are available") The canonical source is either standalone or has no successfully resolved native references.

    Notice(v-if="stale" kind="status")
      Banner(tone="warn")
        strong Disk revision changed.
        |  The visible report is retained until a fresh, internally consistent selection is available.

    ReportView(v-if="report" :report="report" :preview="preview" :status="selectedSummary?.status ?? 'draft'")

    Banner(tone="info")
      strong Native sources remain read-only.
      |  This view reports retained evidence only. Editing, rewrite behavior and Lua output remain outside this workflow.
</template>

<script setup lang="ts" vapor>
import { computed, onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'
import Panel from '#design/panel/Panel.vue'

import { ApiError, fetchImportReports } from './api'
import type { ImportReportEnvelope, ImportReportPreview, ImportReportSummary } from './api'
import { presentApiError } from './formatters'
import { selectionOptions } from './provenance/model'
import ReportView from './provenance/ReportView.vue'
import { useLatestAbortableTask } from './useLatestAbortableTask'
import { plural } from './workspace/plural'
import WorkspaceSection from './workspace/WorkspaceSection.vue'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const reports = ref<ImportReportSummary[]>([])
const selectedIndex = ref<number | null>(null)
const report = ref<ImportReportEnvelope | null>(null)
const preview = ref<ImportReportPreview | null>(null)
const visibleRevision = ref<string | null>(null)
const error = ref<string | null>(null)
const stale = ref(false)
const { loading, run: runLoad, cancel: cancelLoad } = useLatestAbortableTask()

const selectedSummary = computed(() => reports.value.find((summary) => summary.index === selectedIndex.value))
const options = computed(() => selectionOptions(reports.value))

async function load(): Promise<void> {
  error.value = null
  await runLoad(
    async (signal) => {
      const inventory = await fetchImportReports(props.token, undefined, signal)
      signal.throwIfAborted()
      if (inventory.reports.length === 0) return { inventory, selected: null }
      const index = inventory.reports.some((summary) => summary.index === selectedIndex.value)
        ? selectedIndex.value!
        : inventory.reports[0]!.index
      return { inventory, selected: { index, response: await fetchImportReports(props.token, index, signal) } }
    },
    ({ inventory, selected }) => {
      reports.value = inventory.reports
      if (selected === null) return clearReport()
      selectedIndex.value = selected.index
      applySelected(selected.response, false)
    },
    handleRequestError,
  )
}

async function selectReport(value: string): Promise<void> {
  const index = Number(value)
  error.value = null
  selectedIndex.value = index
  await runLoad(
    (signal) => fetchImportReports(props.token, index, signal),
    (response) => applySelected(response, true),
    handleRequestError,
  )
}

function applySelected(next: Awaited<ReturnType<typeof fetchImportReports>>, retainOnRevisionChange: boolean): void {
  if (retainOnRevisionChange && visibleRevision.value !== null && next.diskRevision !== visibleRevision.value) {
    stale.value = true
    return
  }
  report.value = next.report
  preview.value = next.preview
  visibleRevision.value = next.diskRevision
  stale.value = false
}

function clearReport(): void {
  selectedIndex.value = null
  report.value = null
  preview.value = null
  visibleRevision.value = null
  stale.value = false
}

function handleRequestError(requestError: unknown): void {
  if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
  error.value = presentApiError(requestError, 'The import reports route did not respond.')
}

watch(() => props.token, () => {
  cancelLoad()
  reports.value = []
  clearReport()
  error.value = null
  void load()
})

onMounted(load)
</script>
