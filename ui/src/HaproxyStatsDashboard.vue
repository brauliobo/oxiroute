<script setup vapor lang="ts">
import { computed } from 'vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Card from '#design/panel/Card.vue'
import Panel from '#design/panel/Panel.vue'
import type { MonitoringSnapshot } from './api'
import { formatBytes, formatCount, formatTelemetryAge, formatTelemetryDuration, formatTime, pluralize } from './formatters'
import ListenerTable from './overview/ListenerTable.vue'
import MetricList from './overview/MetricList.vue'
import PoolCard from './overview/PoolCard.vue'
import { administrativeStateLabels } from './overview/labels'
import type { Metric } from './overview/metric'
import {
  formatDecimal, formatLimit, formatOptionalBytes, formatOptionalCount, formatOptionalPercent, totalTrafficBytes,
  usedMemoryBytes,
} from './overview/telemetry'
import './overview/styles.css'

const props = defineProps<{ monitoring: MonitoringSnapshot }>()

const headline = computed<Metric[]>(() => [
  { label: 'Active connections', value: formatCount(props.monitoring.traffic.activeConnections) },
  { label: 'Accepted', value: formatCount(props.monitoring.traffic.acceptedConnections) },
  { label: 'Rejected', value: formatCount(props.monitoring.traffic.rejectedConnections) },
  { label: 'Traffic moved', value: formatBytes(totalTrafficBytes(props.monitoring.traffic)) },
])
const processMetrics = computed<Metric[]>(() => {
  const { process, uptimeMs } = props.monitoring
  return [
    { label: 'Administrative state', value: administrativeStateLabels[process.administrativeState] },
    { label: 'Active connections', value: formatCount(process.activeConnections) },
    { label: 'Connection limit', value: formatLimit(process.maxConnections) },
    { label: 'Rejected connections', value: formatCount(process.rejectedConnections) },
    { label: 'Retry attempts', value: formatCount(process.retryAttempts) },
    { label: 'CPU utilization', value: formatOptionalPercent(process.cpuPercent) },
    { label: 'Uptime', value: formatTelemetryDuration(uptimeMs) },
    { label: 'Resident memory', value: formatOptionalBytes(process.residentMemoryBytes) },
    { label: 'Virtual memory', value: formatOptionalBytes(process.virtualMemoryBytes) },
    { label: 'Threads', value: formatOptionalCount(process.threadCount) },
    { label: 'Open files', value: formatOptionalCount(process.openFileDescriptors) },
  ]
})
const trafficMetrics = computed<Metric[]>(() => [
  { label: 'Active connections', value: formatCount(props.monitoring.traffic.activeConnections) },
  { label: 'Accepted connections', value: formatCount(props.monitoring.traffic.acceptedConnections) },
  { label: 'Rejected connections', value: formatCount(props.monitoring.traffic.rejectedConnections) },
  { label: 'Bytes received', value: formatBytes(props.monitoring.traffic.bytesReceived) },
  { label: 'Bytes sent', value: formatBytes(props.monitoring.traffic.bytesSent) },
])
const hostMetrics = computed<Metric[]>(() => {
  const { host } = props.monitoring
  return [
    { label: 'Load average 1 min', value: formatDecimal(host.loadAverage1m) },
    { label: 'Load average 5 min', value: formatDecimal(host.loadAverage5m) },
    { label: 'Load average 15 min', value: formatDecimal(host.loadAverage15m) },
    { label: 'Total memory', value: formatOptionalBytes(host.totalMemoryBytes) },
    { label: 'Available memory', value: formatOptionalBytes(host.availableMemoryBytes) },
    { label: 'Used memory', value: formatOptionalBytes(usedMemoryBytes(host)) },
  ]
})
</script>

<template lang="pug">
Panel.ox-view-section(title="Statistics" data-section="statistics")
  p.ox-muted Snapshot {{ formatTime(monitoring.sampledAtUnixMs) }} ({{ formatTelemetryAge(monitoring.sampledAtUnixMs) }})
  MetricList(large :items="headline")
  .ox-cards
    Card
      h3 Process
      MetricList(:items="processMetrics")
    Card
      h3 Front-door traffic
      MetricList(:items="trafficMetrics")
    Card
      h3 Host
      MetricList(:items="hostMetrics")

Panel.ox-view-section(title="Listeners" data-section="listeners")
  p.ox-muted {{ pluralize(monitoring.listeners.length, 'listener') }}
  EmptyState(v-if="monitoring.listeners.length === 0") No listeners are currently bound.
  ListenerTable(v-else :listeners="monitoring.listeners")

Panel.ox-view-section(title="Upstream pools" data-section="pools")
  p.ox-muted {{ pluralize(monitoring.upstreamPools.length, 'pool') }}
  EmptyState(v-if="monitoring.upstreamPools.length === 0") No upstream pools are configured.
  PoolCard(
    v-for="pool in monitoring.upstreamPools"
    :key="pool.name"
    :pool="pool"
    :sampled-at-unix-ms="monitoring.sampledAtUnixMs"
    detailed
  )
</template>
