<script setup vapor lang="ts">
import { computed } from 'vue'
import StatusBadge from '#design/status/StatusBadge.vue'
import { poolStatusOf } from '#design/status/status'
import EmptyState from '#design/empty/EmptyState.vue'
import Card from '#design/panel/Card.vue'
import type { MonitoringPool } from '../api'
import { formatCount, pluralize } from '../formatters'
import MetricList from './MetricList.vue'
import { poolAlgorithmLabels } from './labels'
import ServerTable from './ServerTable.vue'
import type { Metric } from './metric'
import './styles.css'

const props = defineProps<{ pool: MonitoringPool; sampledAtUnixMs: number; detailed?: boolean }>()

const status = computed(() => poolStatusOf(props.pool.totalEndpoints, props.pool.availableEndpoints))
const summary = computed(() => `${props.pool.availableEndpoints} of ${pluralize(props.pool.totalEndpoints, 'server')} available`)
const unavailableSelections = computed<Metric>(() => ({
  label: 'Unavailable selections',
  value: formatCount(props.pool.unavailableSelections),
}))
const metrics = computed<Metric[]>(() => props.detailed
  ? [
    { label: 'Queued now', value: formatCount(props.pool.queued) },
    { label: 'Queued total', value: formatCount(props.pool.queuedTotal) },
    { label: 'Queue timeouts', value: formatCount(props.pool.queueTimeouts) },
    { label: 'Queue cancellations', value: formatCount(props.pool.queueCancellations) },
    unavailableSelections.value,
  ]
  : [unavailableSelections.value])
</script>

<template lang="pug">
Card.ox-pool
  .ox-card-head
    h3 {{ pool.name }}
    StatusBadge(:status="status")
  p.ox-muted {{ poolAlgorithmLabels[pool.algorithm] }} / {{ summary }}
  MetricList(:items="metrics")
  EmptyState(v-if="pool.endpoints.length === 0") This pool has no servers.
  ServerTable(
    v-else
    :pool-name="pool.name"
    :servers="pool.endpoints"
    :sampled-at-unix-ms="sampledAtUnixMs"
    :detailed="detailed"
  )
</template>
