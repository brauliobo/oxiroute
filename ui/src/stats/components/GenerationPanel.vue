<script setup vapor lang="ts">
import { computed } from 'vue'
import { formatDuration } from '../format'
import { useStatsStore } from '../useStatsStore'
import DetailList from './DetailList.vue'
import type { Detail } from './present'
import * as cell from './present'
import StatusBadge from './StatusBadge.vue'
import './panels.css'

// The running configuration generation: revisions, counters, the last failure and rejection, and the generations still
// draining. Revisions are only sent to clients allowed to see them.
const { model } = useStatsStore()
const wire = computed(() => model.value!.generation.wire)
const entry = (label: string, value: bigint | number | undefined): Detail => ({ label, value: cell.exact(value) })
const revisions = computed<Detail[]>(() => [
  { label: 'Active', value: wire.value.activeRevision }, { label: 'Candidate', value: wire.value.candidateRevision },
  { label: 'Previous', value: wire.value.previousRevision }, { label: 'On disk', value: wire.value.diskRevision },
  { label: 'Quarantined', value: wire.value.quarantinedRevision },
])
const counters = computed<Detail[]>(() => [
  { label: 'Age', value: cell.duration(wire.value.ageMs) }, entry('Activations', wire.value.activations),
  entry('Prepares', wire.value.prepares), entry('Rollbacks', wire.value.rollbacks), entry('Failures', wire.value.failures),
  entry('Configuration rejections', wire.value.configRejections), { label: 'Last failure', value: wire.value.lastFailure },
  entry('Retired kept', wire.value.maxRetiredGenerations),
])
const rejection = computed(() => wire.value.lastRejection)
const kinds = (retired: { inFlightByKind: Record<string, number> }) =>
  Object.entries(retired.inFlightByKind).map(([kind, total]) => `${kind} ${total}`).join(', ')
</script>

<template lang="pug">
section.ox-panel#generation(v-if="model" aria-labelledby="ox-generation-title")
  h2#ox-generation-title Generation {{ wire.number }}
  p.ox-badges
    StatusBadge(:status="wire.accepting ? 'ok' : 'down'" :label="wire.accepting ? 'ACCEPTING' : 'NOT ACCEPTING'")
    StatusBadge(:status="wire.degraded ? 'degraded' : 'healthy'" :label="wire.degraded ? 'DEGRADED' : 'HEALTHY'")
  h3 Revisions
  DetailList(:items="revisions")
  h3 History
  DetailList(:items="counters")
  template(v-if="rejection")
    h3 Last rejection
    p {{ cell.timestamp(rejection.atUnixMs) }}, revision {{ cell.text(rejection.revision) }}
    ul.ox-diagnostics
      li(v-for="item in rejection.diagnostics" :key="item.code + item.stage")
        code {{ item.code }}
        |  ({{ item.stage }}) {{ item.message }}
  template(v-if="wire.retired.length")
    h3 Draining generations
    table.ox-table.ox-plain
      caption.ox-sr-only Retired generations that still hold connections
      thead
        tr
          th(scope="col") Revision
          th(scope="col") State
          th(scope="col") Age
          th(scope="col") Deadline
          th(scope="col") In flight
      tbody.ox-row(v-for="item in wire.retired" :key="item.revision")
        tr
          th.ox-name(scope="row" data-label="Revision") {{ item.revision }}
          td(data-label="State") {{ item.state }}
          td(data-label="Age") {{ cell.duration(item.ageSeconds * 1000) }}
          td(data-label="Deadline") {{ cell.duration(item.deadlineRemainingSeconds * 1000) }}
          td(data-label="In flight")
            span {{ item.inFlight }}
            span.ox-sub {{ kinds(item) }}
</template>
