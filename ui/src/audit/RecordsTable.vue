<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Table from '#design/table/Table.vue'
import type { AuditRecord } from '../api'
import { sentence } from '../events/format'
import { formatTime, shortRevision } from '../formatters'
import ScrollRegion from '../workspace/ScrollRegion.vue'
import { resultTone } from './model'

defineProps<{ records: readonly AuditRecord[] }>()
const COLUMNS = ['Record', 'Time', 'Operation', 'Result', 'Category', 'Actor', 'Source', 'Correlation', 'Revision']
</script>

<template lang="pug">
ScrollRegion(label="Audit records table")
  Table(caption="Redacted durable audit records" :columns="COLUMNS")
    tbody
      tr(v-for="record in records" :key="record.id")
        th.ox-name(scope="row") {{ record.id }}
        td(data-label="Time") {{ formatTime(record.timestampUnixMs) }}
        td(data-label="Operation") {{ record.operation }}
        td(data-label="Result")
          Badge(:tone="resultTone(record.result)" :label="sentence(record.result)")
        td(data-label="Category") {{ sentence(record.category) }}
        td(data-label="Actor") {{ record.actor }}
        td(data-label="Source") {{ record.source }}
        td.ox-mono(data-label="Correlation") {{ record.correlationId }}
        td.ox-mono(data-label="Revision") {{ record.revision ? shortRevision(record.revision) : '-' }}
</template>
