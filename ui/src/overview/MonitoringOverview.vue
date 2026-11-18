<script setup vapor lang="ts">
import { computed } from 'vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Meter from '#design/meter/Meter.vue'
import Card from '#design/panel/Card.vue'
import Panel from '#design/panel/Panel.vue'
import type { MonitoringSnapshot } from '../api'
import { formatBytes, formatCount, formatTime, pluralize } from '../formatters'
import ListenerTable from './ListenerTable.vue'
import MetricList from './MetricList.vue'
import PoolCard from './PoolCard.vue'
import type { Metric } from './metric'
import {
  formatDecimal, formatOptionalBytes, formatOptionalCount, formatOptionalPercent, formatSampleAge, memoryUsagePercent,
  usedMemoryBytes,
} from './telemetry'
import './styles.css'

const props = defineProps<{ monitoring: MonitoringSnapshot; currentUnixMs: number }>()

const hostLoads = computed(() => [
  { label: '1 min', value: props.monitoring.host.loadAverage1m },
  { label: '5 min', value: props.monitoring.host.loadAverage5m },
  { label: '15 min', value: props.monitoring.host.loadAverage15m },
])
const maximumHostLoad = computed(() => Math.max(1, ...hostLoads.value.map(({ value }) => value ?? 0)))
const memoryPercent = computed(() => memoryUsagePercent(props.monitoring.host))
const memoryText = computed(() => formatOptionalPercent(memoryPercent.value))

const traffic = computed<Metric[]>(() => [
  { label: 'Active connections', value: formatCount(props.monitoring.traffic.activeConnections) },
  { label: 'Accepted in total', value: formatCount(props.monitoring.traffic.acceptedConnections) },
  { label: 'Received', value: formatBytes(props.monitoring.traffic.bytesReceived) },
  { label: 'Sent', value: formatBytes(props.monitoring.traffic.bytesSent) },
])
const memory = computed<Metric[]>(() => [
  { label: 'Used', value: formatOptionalBytes(usedMemoryBytes(props.monitoring.host)) },
  { label: 'Total', value: formatOptionalBytes(props.monitoring.host.totalMemoryBytes) },
  { label: 'Available', value: formatOptionalBytes(props.monitoring.host.availableMemoryBytes) },
])
const runtime = computed<Metric[]>(() => [
  { label: 'CPU utilization', value: formatOptionalPercent(props.monitoring.process.cpuPercent) },
  { label: 'Resident memory', value: formatOptionalBytes(props.monitoring.process.residentMemoryBytes) },
  { label: 'Virtual memory', value: formatOptionalBytes(props.monitoring.process.virtualMemoryBytes) },
  { label: 'Threads', value: formatOptionalCount(props.monitoring.process.threadCount) },
  { label: 'Open files', value: formatOptionalCount(props.monitoring.process.openFileDescriptors) },
])
const rtmp = computed<Metric[]>(() => [
  { label: 'Active streams', value: formatCount(props.monitoring.rtmp.activeStreams) },
  { label: 'Publishers', value: formatCount(props.monitoring.rtmp.publishers) },
  { label: 'Subscribers', value: formatCount(props.monitoring.rtmp.subscribers) },
  { label: 'Media received', value: formatBytes(props.monitoring.rtmp.mediaPayloadBytesReceived) },
])
</script>

<template lang="pug">
Panel.ox-view-section(title="Monitoring overview" data-section="monitoring")
  p.ox-muted Snapshot {{ formatTime(monitoring.sampledAtUnixMs) }} ({{ formatSampleAge(currentUnixMs, monitoring.sampledAtUnixMs) }})
  .ox-cards
    Card(data-section="traffic")
      h3 Traffic
      MetricList(large :items="traffic")
    Card(data-section="host")
      h3 Host load and memory
      .ox-loads(role="group" aria-label="Host load averages")
        .ox-load(v-for="load in hostLoads" :key="load.label")
          span {{ load.label }}
          Meter(
            :value="load.value ?? 0"
            :max="maximumHostLoad"
            :label="`Load average ${load.label}`"
            :value-text="formatDecimal(load.value)"
          )
          strong {{ formatDecimal(load.value) }}
      .ox-load
        span Memory
        Meter(:value="memoryPercent ?? 0" label="Host memory used" :value-text="memoryText")
        strong {{ memoryText }}
      MetricList(:items="memory")
    Card(data-section="process")
      h3 OxiRoute process
      MetricList(:items="runtime")
    Card(data-section="rtmp")
      h3 RTMP media
      MetricList(:items="rtmp")

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
  )
</template>
