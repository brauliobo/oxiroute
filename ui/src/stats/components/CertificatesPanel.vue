<script setup vapor lang="ts">
import { computed } from 'vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import { useStatsStore } from '../useStatsStore'
import * as cell from './present'
import StatusBadge from './StatusBadge.vue'
import './dashboard.css'

// Certificates by expiry, and the watchers that renew them.
const CERTIFICATE_COLUMNS = ['Name', 'Expiry', 'Expires', 'Source', 'Last outcome']
const WATCHER_COLUMNS = ['Source', 'Health', 'Backend errors', 'Reconciliation failures']
const { model } = useStatsStore()
const sorted = computed(() => [...(model.value?.certificates ?? [])].sort((left, right) =>
  (left.remainingMs ?? Infinity) - (right.remainingMs ?? Infinity)))
</script>

<template lang="pug">
Panel#certificates(v-if="model" title="Certificates")
  EmptyState(v-if="sorted.length === 0") No certificates are loaded.
  Table.ox-plain(v-else caption="Certificates, soonest expiry first" :columns="CERTIFICATE_COLUMNS")
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
    Table.ox-plain(caption="Certificate renewal watchers" :columns="WATCHER_COLUMNS")
      tbody.ox-row(v-for="item in model.certificateWatchers" :key="item.key")
        tr
          th.ox-name(scope="row" data-label="Source") {{ cell.words(item.wire.source) }}
          td(data-label="Health")
            StatusBadge(:status="item.wire.health")
          td(data-label="Backend errors") {{ cell.exact(item.wire.backendErrors) }}
          td(data-label="Reconciliation failures") {{ cell.exact(item.wire.reconciliationFailures) }}
</template>
