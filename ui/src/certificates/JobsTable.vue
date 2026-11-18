<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Table from '#design/table/Table.vue'
import type { OperationalEvent } from '../api'
import { outcomeLabel, outcomeTone, sentence } from '../events/format'
import { formatTime, shortRevision } from '../formatters'
import ScrollRegion from '../workspace/ScrollRegion.vue'

defineProps<{ jobs: readonly OperationalEvent[] }>()
const COLUMNS = ['Cursor', 'Time', 'Certificate', 'Event', 'Outcome', 'Revision']
</script>

<template lang="pug">
ScrollRegion(label="Certificate jobs table")
  Table(caption="Certificate jobs retained in the event ring" :columns="COLUMNS")
    tbody
      tr(v-for="job in jobs" :key="job.cursor")
        th.ox-name(scope="row") {{ job.cursor }}
        td(data-label="Time") {{ job.timestampUnixMs === null ? 'No timestamp' : formatTime(job.timestampUnixMs) }}
        td(data-label="Certificate") {{ job.certificate }}
        td(data-label="Event") {{ sentence(job.event) }}
        td(data-label="Outcome")
          Badge(:tone="outcomeTone(job.outcome)" :label="outcomeLabel(job.outcome)")
        td.ox-mono(data-label="Revision") {{ job.revision ? shortRevision(job.revision) : '-' }}
</template>
