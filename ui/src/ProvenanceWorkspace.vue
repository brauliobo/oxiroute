<template lang="pug">
section.provenance-workspace(aria-labelledby="provenance-heading" :aria-busy="loading")
  header.workspace-heading
    div
      p.eyebrow Native import evidence
      h2#provenance-heading Provenance
      p.workspace-deck Inspect bounded, redacted evidence retained by the native resolver. Source files remain read-only and canonical configuration remains the only editable surface.
    button.secondary-button(type="button" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh reports' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to inspect native import evidence.

  .loading-panel(v-else-if="loading && !report" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading import reports
      p No source evidence is rendered until the authenticated contract responds.

  p.notice.error-notice(v-if="error" role="alert")
    strong Provenance unavailable.
    |  {{ error }}

  template(v-if="(token && !loading) || report")
    section.selection-panel(v-if="reports.length" aria-labelledby="report-selection-heading")
      .section-heading
        div
          p.eyebrow Report inventory
          h3#report-selection-heading Select a native source
        span.panel-note {{ reports.length }} {{ reports.length === 1 ? 'report' : 'reports' }} retained
      label.selection-label(for="import-report-selection") Native import report
      select#import-report-selection(v-model.number="selectedIndex" :disabled="loading" @change="selectReport")
        option(v-for="summary in reports" :key="summary.index" :value="summary.index")
          | {{ summary.product }} / {{ summary.capabilityProfile.id }} / {{ summary.status }}

    section.empty-panel(v-else-if="!loading" role="status")
      p.eyebrow No native references
      h3 No import reports are available
      p The canonical source is either standalone or has no successfully resolved native references.

    template(v-if="report")
      p.stale-banner(v-if="stale" role="status")
        strong disk revision changed.
        |  The visible report is retained until a fresh, internally consistent selection is available.

      section.report-panel(aria-labelledby="report-heading")
        header.report-heading
          div.report-identity
            p.eyebrow {{ report.source.product }} import
            h3#report-heading {{ report.source.capabilityProfile.id }}
            p {{ report.source.version ? `Source version ${report.source.version}` : 'Source version not supplied' }}
          span.status-chip(:class="`status-${selectedSummary?.status ?? 'draft'}`") {{ selectedSummary?.status ?? 'draft' }}
        .report-metrics
          .metric
            span.label Sources
            strong {{ report.sourceGraph.sources.length }}
          .metric
            span.label Dependencies
            strong {{ report.sourceGraph.dependencies.length }}
          .metric
            span.label Provenance paths
            strong {{ report.candidate.provenance.length }}
          .metric
            span.label Requirements
            strong {{ requirementCount }}
          .metric
            span.label Diagnostics
            strong {{ report.diagnostics.length }}

      section.evidence-panel.panel(aria-labelledby="evidence-heading")
        header.panel-heading
          div
            p.eyebrow Source graph
            h3#evidence-heading Resolved inputs
          span.panel-note {{ report.sourceGraph.snapshotStable === false ? 'Snapshot changed during import' : 'Paths redacted at the API boundary' }}
        ul.source-list
          li(v-for="source in report.sourceGraph.sources" :key="source.id")
            div
              strong Source {{ source.id }}
              small {{ source.name }}
            code {{ source.byteLength }} bytes
        p.empty-list(v-if="report.sourceGraph.sources.length === 0") No source graph entries were retained.
        ul.evidence-list(v-if="report.sourceGraph.dependencies.length")
          li(v-for="(dependency, index) in report.sourceGraph.dependencies" :key="`${dependency.sourceId}-${dependency.targetSourceId}-${index}`")
            code {{ dependency.kind }}
            div
              strong Source {{ dependency.sourceId }} -> {{ dependency.targetSourceId === null ? 'unresolved' : `source ${dependency.targetSourceId}` }}
              small {{ dependency.status }}{{ dependency.truncated ? ' / truncated' : '' }}

      section.blockers-panel.panel(v-if="report.blockers.length" aria-labelledby="blockers-heading")
        header.panel-heading
          div
            p.eyebrow Conversion boundary
            h3#blockers-heading Blockers
          span.panel-note Exact conversion is not claimed
        ul.evidence-list
          li(v-for="blocker in report.blockers" :key="blocker.id")
            code {{ blocker.code }}
            div
              strong {{ blocker.message }}
              small {{ blocker.scope || blocker.kind }}

      section.requirements-panel.panel(aria-labelledby="requirements-heading")
        header.panel-heading
          div
            p.eyebrow Deployment and activation
            h3#requirements-heading Requirements
          span.panel-note {{ requirementCount }} retained
        ul.evidence-list(v-if="requirements.length")
          li(v-for="(requirement, index) in requirements" :key="`${requirement.directive}-${index}`")
            code {{ requirement.directive }}
            div
              strong {{ requirement.kind }}
              small {{ requirement.values.length ? `${requirement.values.length} value${requirement.values.length === 1 ? '' : 's'} redacted` : 'No values supplied' }}
        p.empty-list(v-else) No deployment or activation requirements were returned.

      section.provenance-panel.panel(aria-labelledby="provenance-evidence-heading")
        header.panel-heading
          div
            p.eyebrow Canonical field origins
            h3#provenance-evidence-heading Provenance
          span.panel-note {{ report.candidate.provenance.length }} fields
        ul.evidence-list(v-if="report.candidate.provenance.length")
          li(v-for="entry in report.candidate.provenance" :key="entry.path")
            code {{ entry.path }}
            div
              strong {{ entry.origins.length }} {{ entry.origins.length === 1 ? 'origin' : 'origins' }}
              small(v-if="entry.origins.length") {{ originLabel(entry.origins[0]) }}
        p.empty-list(v-else) No canonical field provenance was retained.

      section.overlays-panel.panel(v-if="report.overlays.length" aria-labelledby="overlays-heading")
        header.panel-heading
          div
            p.eyebrow Operational overlays
            h3#overlays-heading Overlays
          span.panel-note Evidence values are redacted
        ul.evidence-list
          li(v-for="overlay in report.overlays" :key="overlay.id")
            code {{ overlay.kind }}
            div
              strong {{ overlay.id }}
              small {{ overlay.satisfied ? 'Satisfied' : 'Not satisfied' }}

      section.diagnostics-panel.panel(aria-labelledby="provenance-diagnostics-heading")
        header.panel-heading
          div
            p.eyebrow Import diagnostics
            h3#provenance-diagnostics-heading Diagnostics
          span.panel-note {{ report.diagnostics.length }}
        p.empty-list(v-if="report.diagnostics.length === 0") No import diagnostics were returned.
        ul.evidence-list(v-else)
          li(v-for="(diagnostic, index) in report.diagnostics" :key="`${diagnostic.code}-${index}`")
            code {{ diagnostic.code }}
            div
              strong {{ diagnostic.message }}
              small {{ diagnostic.stage }} / {{ diagnostic.severity }}

      section.preview-panel.panel(v-if="preview" aria-labelledby="preview-heading")
        header.panel-heading
          div
            p.eyebrow Canonical preview
            h3#preview-heading Read-only KDL preview
          span.panel-note Candidate is finalized and unblocked
        pre {{ preview.text }}

      aside.blocked-preview(v-else-if="report.blockers.length" role="note")
        strong No preview was produced.
        p The candidate is blocked, so the UI does not imply an exact canonical conversion.

      aside.boundary-note(role="note")
        strong Native sources remain read-only.
        p This view reports retained evidence only. Editing, rewrite behavior, and Lua output remain outside this workflow.

    template(v-else-if="!loading")
      aside.boundary-note(role="note")
        strong Native sources remain read-only.
        p This view reports retained evidence only. Editing, rewrite behavior, and Lua output remain outside this workflow.
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { ApiError, fetchImportReports } from './api'
import type {
  ImportReportEnvelope,
  ImportReportOrigin,
  ImportReportPreview,
  ImportReportRequirement,
  ImportReportSummary,
} from './api'
import { useLatestAbortableTask } from './useLatestAbortableTask'

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
const requirements = computed<ImportReportRequirement[]>(() => [
  ...(report.value?.requirements.deployment ?? []),
  ...(report.value?.requirements.activation ?? []),
])
const requirementCount = computed(() => requirements.value.length)

async function load(): Promise<void> {
  const token = props.token
  if (!token) return
  error.value = null
  await runLoad(
    async (signal) => {
      const inventory = await fetchImportReports(token, undefined, signal)
      signal.throwIfAborted()
      if (inventory.reports.length === 0) return { inventory, selected: null }
      const index = inventory.reports.some((summary) => summary.index === selectedIndex.value)
        ? selectedIndex.value!
        : inventory.reports[0]!.index
      const selected = await loadSelected(index, token, signal)
      return { inventory, selected: { index, response: selected } }
    },
    ({ inventory, selected }) => {
      reports.value = inventory.reports
      if (selected === null) {
        selectedIndex.value = null
        report.value = null
        preview.value = null
        visibleRevision.value = null
        stale.value = false
        return
      }
      selectedIndex.value = selected.index
      applySelected(selected.response, false)
    },
    handleRequestError,
  )
}

async function selectReport(): Promise<void> {
  const index = selectedIndex.value
  const token = props.token
  if (index === null || !token) return
  error.value = null
  await runLoad(
    (signal) => loadSelected(index, token, signal),
    (response) => applySelected(response, true),
    handleRequestError,
  )
}

function loadSelected(index: number, token: string, signal: AbortSignal) {
  return fetchImportReports(token, index, signal)
}

function applySelected(next: Awaited<ReturnType<typeof loadSelected>>, retainOnRevisionChange: boolean): void {
  if (retainOnRevisionChange && visibleRevision.value !== null && next.diskRevision !== visibleRevision.value) {
    stale.value = true
    return
  }
  report.value = next.report
  preview.value = next.preview
  visibleRevision.value = next.diskRevision
  stale.value = false
}

function handleRequestError(requestError: unknown): void {
  if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
  error.value = requestError instanceof Error ? requestError.message : 'The import reports route did not respond.'
}

function originLabel(origin: ImportReportOrigin): string {
  const role = origin.role ?? 'source origin'
  return `${role} / source ${origin.sourceId}`
}

watch(() => props.token, (token) => {
  cancelLoad()
  if (!token) {
    reports.value = []
    selectedIndex.value = null
    report.value = null
    preview.value = null
    visibleRevision.value = null
    stale.value = false
    error.value = null
    return
  }
  void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>

<style scoped>
.provenance-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.section-heading,
.panel-heading,
.report-heading {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 18px;
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
  max-width: 760px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.empty-panel,
.panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel,
.empty-panel {
  min-height: 190px;
  padding: clamp(24px, 5vw, 48px);
}

.auth-panel,
.loading-panel {
  display: flex;
  align-items: center;
  gap: 22px;
}

.auth-panel p,
.loading-panel p,
.empty-panel p {
  margin: 8px 0 0;
  color: #8e9686;
  line-height: 1.5;
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

.notice,
.stale-banner,
.blocked-preview,
.boundary-note {
  margin: 18px 0 0;
  padding: 14px 17px;
  border-left: 3px solid #ffbf4b;
  background: #1c1c15;
  color: #cdd2c5;
}

.error-notice {
  border-left-color: #ff745c;
  background: #171a15;
}

.stale-banner {
  border-left-color: #ff745c;
}

.blocked-preview {
  border-left-color: #ff745c;
}

.boundary-note p,
.blocked-preview p {
  margin: 7px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.selection-panel,
.report-panel,
.panel,
.blocked-preview,
.boundary-note {
  margin-top: 22px;
}

.selection-panel {
  padding: 20px;
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.panel-note,
.empty-list,
.source-list small,
.evidence-list small,
.metric strong,
.report-heading p {
  color: #8e9686;
  font-size: 0.72rem;
}

.selection-label {
  display: block;
  margin: 20px 0 7px;
  color: #aeb8a6;
  font-size: 0.78rem;
  font-weight: 700;
}

select {
  width: 100%;
  min-height: 42px;
  padding: 9px 11px;
  border: 1px solid #56604f;
  color: #dce3d4;
  background: #0e110d;
  font: 0.78rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.report-panel {
  padding: 22px;
  border: 1px solid #5b7269;
  background: #111710;
}

.report-heading {
  align-items: flex-start;
}

.report-heading p {
  margin: 8px 0 0;
}

.status-chip {
  padding: 6px 9px;
  border: 1px solid #806f47;
  color: #ffbf4b;
  font: 700 0.65rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: uppercase;
}

.status-finalized {
  border-color: #5b8962;
  color: #b6ff51;
}

.status-blocked {
  border-color: #8b5146;
  color: #ff907e;
}

.report-metrics {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 1px;
  margin-top: 22px;
  background: #34392f;
}

.metric {
  display: grid;
  gap: 8px;
  min-width: 0;
  padding: 15px;
  background: #0e110d;
}

.metric strong {
  color: #b6ff51;
  font: 1.45rem Georgia, "Times New Roman", serif;
}

.panel-heading {
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.source-list,
.evidence-list {
  margin: 0;
  padding: 0;
  list-style: none;
}

.source-list li,
.evidence-list li {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 18px;
  padding: 16px 20px;
  border-bottom: 1px solid #292e26;
  background: #10130e;
}

.source-list div,
.evidence-list div {
  display: grid;
  gap: 6px;
  min-width: 0;
}

.source-list strong,
.evidence-list strong {
  color: #dce3d4;
}

.source-list small,
.evidence-list small {
  line-height: 1.4;
}

code,
pre {
  overflow-wrap: anywhere;
  color: #dce3d4;
  font: 0.72rem/1.5 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.empty-list {
  margin: 0;
  padding: 22px 20px;
}

.preview-panel pre {
  max-height: 460px;
  margin: 0;
  padding: 20px;
  overflow: auto;
  background: #0a0d0a;
  color: #c9e89a;
  white-space: pre-wrap;
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

@keyframes provenance-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 900px) {
  .report-metrics {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .section-heading,
  .panel-heading,
  .report-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .auth-panel,
  .loading-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .report-metrics {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .source-list li,
  .evidence-list li {
    grid-template-columns: 1fr;
    gap: 8px;
  }
}

@media (max-width: 420px) {
  .report-metrics {
    grid-template-columns: 1fr;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
