<script setup vapor lang="ts">
import { computed } from 'vue'
import { useStatsStore } from '../useStatsStore'
import * as cell from './present'
import StatusBadge from './StatusBadge.vue'
import './panels.css'

// Certificates by expiry, and the watchers that renew them.
const { model } = useStatsStore()
const sorted = computed(() => [...(model.value?.certificates ?? [])].sort((left, right) =>
  (left.remainingMs ?? Infinity) - (right.remainingMs ?? Infinity)))
</script>

<template lang="pug">
section.ox-panel#certificates(v-if="model" aria-labelledby="ox-certificates-title")
  h2#ox-certificates-title Certificates
  p.ox-muted(v-if="sorted.length === 0") No certificates are loaded.
  table.ox-table.ox-plain(v-else)
    caption.ox-sr-only Certificates, soonest expiry first
    thead
      tr
        th(scope="col") Name
        th(scope="col") Expiry
        th(scope="col") Expires
        th(scope="col") Source
        th(scope="col") Last outcome
    tbody.ox-row(v-for="item in sorted" :key="item.key")
      tr
        th.ox-name(scope="row" data-label="Name") {{ item.name }}
        td(data-label="Expiry")
          StatusBadge(:status="item.severity" :label="cell.certificateLabel(item.severity)")
        td(data-label="Expires")
          span {{ cell.remaining(item.remainingMs) }}
          span.ox-sub {{ item.wire.expiresAt }}
        td(data-label="Source") {{ cell.words(item.wire.source) }}
        td(data-label="Last outcome")
          span {{ cell.text(item.wire.lastOutcome) }}
          span.ox-sub(v-if="item.wire.lastErrorCode") {{ item.wire.lastErrorCode }}
  template(v-if="model.certificateWatchers.length")
    h3 Renewal watchers
    table.ox-table.ox-plain
      caption.ox-sr-only Certificate renewal watchers
      thead
        tr
          th(scope="col") Source
          th(scope="col") Health
          th(scope="col") Backend errors
          th(scope="col") Reconciliation failures
      tbody.ox-row(v-for="item in model.certificateWatchers" :key="item.key")
        tr
          th.ox-name(scope="row" data-label="Source") {{ cell.words(item.wire.source) }}
          td(data-label="Health")
            StatusBadge(:status="item.wire.health")
          td(data-label="Backend errors") {{ cell.exact(item.wire.backendErrors) }}
          td(data-label="Reconciliation failures") {{ cell.exact(item.wire.reconciliationFailures) }}
</template>
