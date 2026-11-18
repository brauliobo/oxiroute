<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Table from '#design/table/Table.vue'
import type { OperationalEvent } from '../api'
import { formatTime } from '../formatters'
import ScrollRegion from '../workspace/ScrollRegion.vue'
import { eventNotes, outcomeLabel, outcomeTone, sentence } from './format'

defineProps<{ events: readonly OperationalEvent[] }>()
const COLUMNS = ['Cursor', 'Time', 'Event', 'Outcome', 'Details']
</script>

<template lang="pug">
ScrollRegion(label="Events table")
  Table(caption="Operational events retained in the event ring" :columns="COLUMNS")
    tbody
      tr(v-for="event in events" :key="event.cursor")
        th.ox-name(scope="row") {{ event.cursor }}
        td(data-label="Time") {{ event.timestampUnixMs === null ? 'No timestamp' : formatTime(event.timestampUnixMs) }}
        td(data-label="Event") {{ sentence(event.event) }}
        td(data-label="Outcome")
          Badge(:tone="outcomeTone(event.outcome)" :label="outcomeLabel(event.outcome)")
        td(data-label="Details")
          div(v-for="note in eventNotes(event)" :key="note") {{ note }}
</template>
