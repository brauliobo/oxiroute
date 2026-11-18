<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Panel from '#design/panel/Panel.vue'
import type { ImportReportEnvelope, ImportReportPreview, ImportReportSummary } from '../api'
import Facts from '../workspace/Facts.vue'
import ScrollRegion from '../workspace/ScrollRegion.vue'
import EvidencePanel from './EvidencePanel.vue'
import {
  blockerRows,
  dependencyRows,
  diagnosticRows,
  originRows,
  overlayRows,
  reportMetrics,
  requirementRows,
  sourceRows,
  statusBadge,
  versionText,
} from './model'
import './styles.css'

const props = defineProps<{
  report: ImportReportEnvelope
  preview: ImportReportPreview | null
  status: ImportReportSummary['status']
}>()

const badge = computed(() => statusBadge(props.status))
const title = computed(() => `${props.report.source.product} import: ${props.report.source.capabilityProfile.id}`)
const requirements = computed(() => requirementRows(props.report))
const snapshotNote = computed(() =>
  props.report.sourceGraph.snapshotStable === false ? 'Snapshot changed during import.' : 'Paths are redacted at the API boundary.')
</script>

<template lang="pug">
.ox-stack
  Panel(:title="title")
    .ox-stack
      p
        Badge(:tone="badge.tone" :label="badge.label")
        |  {{ versionText(report) }}
      Facts(:items="reportMetrics(report)")

  EvidencePanel(
    title="Resolved inputs"
    :note="snapshotNote"
    caption="Source files"
    :columns="['Source', 'Name', 'Size']"
    :rows="sourceRows(report)"
    empty="No source graph entries were retained."
  )
  EvidencePanel(
    v-if="report.sourceGraph.dependencies.length"
    title="Source dependencies"
    note="How the source files include each other."
    caption="Source dependencies"
    :columns="['Kind', 'From', 'To', 'Status']"
    :rows="dependencyRows(report)"
    empty="No source dependencies were retained."
  )
  EvidencePanel(
    v-if="report.blockers.length"
    title="Blockers"
    note="Exact conversion is not claimed."
    caption="Conversion blockers"
    :columns="['Code', 'Message', 'Scope']"
    :rows="blockerRows(report)"
    empty="No blockers were returned."
  )
  EvidencePanel(
    title="Requirements"
    :note="`${requirements.length} retained from the deployment and activation requirements.`"
    caption="Deployment and activation requirements"
    :columns="['Directive', 'Kind', 'Values']"
    :rows="requirements"
    empty="No deployment or activation requirements were returned."
  )
  EvidencePanel(
    title="Canonical field origins"
    :note="`${report.candidate.provenance.length} canonical fields trace back to a source.`"
    caption="Canonical field origins"
    :columns="['Field', 'Origins', 'First origin']"
    :rows="originRows(report)"
    empty="No canonical field provenance was retained."
  )
  EvidencePanel(
    v-if="report.overlays.length"
    title="Operational overlays"
    note="Evidence values are redacted."
    caption="Operational overlays"
    :columns="['Overlay', 'Kind', 'State']"
    :rows="overlayRows(report)"
    empty="No overlays were returned."
  )
  EvidencePanel(
    title="Diagnostics"
    :note="`${report.diagnostics.length} returned by the import.`"
    caption="Import diagnostics"
    :columns="['Code', 'Message', 'Stage', 'Severity']"
    :rows="diagnosticRows(report)"
    empty="No import diagnostics were returned."
  )

  Panel(v-if="preview" title="Read-only KDL preview")
    .ox-stack
      p.ox-muted Candidate is finalized and unblocked.
      ScrollRegion.ox-preview(label="KDL preview text")
        pre.ox-mono {{ preview.text }}
  Banner(v-else-if="report.blockers.length" tone="warn")
    strong No preview was produced.
    |  The candidate is blocked, so the UI does not imply an exact canonical conversion.
</template>
