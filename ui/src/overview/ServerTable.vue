<script setup vapor lang="ts">
import { computed } from 'vue'
import StatusBadge from '#design/status/StatusBadge.vue'
import { serverStatusOf } from '#design/status/status'
import Table from '#design/table/Table.vue'
import type { MonitoringPoolEndpoint } from '../api'
import { formatCount, formatTelemetryAge } from '../formatters'
import { administrativeStateLabels, healthFailureLabels, healthOverrideLabels } from './labels'
import { formatLimit } from './telemetry'
import './styles.css'

const props = defineProps<{
  poolName: string
  servers: readonly MonitoringPoolEndpoint[]
  sampledAtUnixMs: number
  detailed?: boolean
}>()

const BASE_COLUMNS = ['Server', 'Status', 'Connections', 'Checks', 'Observation']
const columns = computed(() => (props.detailed ? [...BASE_COLUMNS, 'Administration', 'Override'] : BASE_COLUMNS))
const rowKey = (server: MonitoringPoolEndpoint) => `${props.poolName}:${server.name}:${server.address}`
const ago = (unixMs: number) => formatTelemetryAge(unixMs, props.sampledAtUnixMs)
const observation = (server: MonitoringPoolEndpoint) =>
  server.lastCheckedAtUnixMs === null ? 'No checks completed' : `Checked ${ago(server.lastCheckedAtUnixMs)}`
const transition = (server: MonitoringPoolEndpoint) =>
  server.lastTransitionAtUnixMs === null ? 'No transition yet' : `Changed ${ago(server.lastTransitionAtUnixMs)}`
const checkState = (server: MonitoringPoolEndpoint) =>
  `${server.checksEnabled ? 'Checks on' : 'Checks off'}, ${server.checksRunning ? 'running' : 'not running'}`
</script>

<template lang="pug">
Table(:caption="`${poolName} servers`" :columns="columns")
  tbody.ox-row(v-for="server in servers" :key="rowKey(server)")
    tr
      th.ox-name(scope="row" data-label="Server")
        | {{ server.name }}
        span.ox-cell-note {{ server.address }}
      td(data-label="Status")
        StatusBadge(:status="serverStatusOf(server)")
        span.ox-cell-note(v-if="server.lastFailure") Last failure: {{ healthFailureLabels[server.lastFailure] }}
      td(data-label="Connections") {{ formatCount(server.activeConnections) }} / {{ formatLimit(server.maxConnections) }}
      td(data-label="Checks")
        | {{ formatCount(server.successfulChecks) }} passed / {{ formatCount(server.failedChecks) }} failed
        span.ox-cell-note {{ formatCount(server.consecutiveSuccesses) }} passed / {{ formatCount(server.consecutiveFailures) }} failed in a row
      td(data-label="Observation")
        | {{ observation(server) }}
        span.ox-cell-note {{ transition(server) }}
      td(v-if="detailed" data-label="Administration")
        | {{ administrativeStateLabels[server.administrativeState] }}
        span.ox-cell-note {{ checkState(server) }}
      td(v-if="detailed" data-label="Override")
        | {{ healthOverrideLabels[server.healthOverride] }}
        span.ox-cell-note Configured limit {{ formatLimit(server.configuredMaxConnections) }}
</template>
