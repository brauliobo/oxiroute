<script setup vapor lang="ts">
import { computed } from 'vue'
import { useStatsStore } from '../useStatsStore'
import DetailList from './DetailList.vue'
import type { Detail } from './present'
import * as cell from './present'
import StatusBadge from './StatusBadge.vue'
import './panels.css'

// The proxy process and its host: resources, connections and work in flight.
const { model } = useStatsStore()
const wire = computed(() => model.value!.process.wire)
const entry = (label: string, value: bigint | number | undefined): Detail => ({ label, value: cell.exact(value) })
const load = (value?: number) => (value === undefined ? undefined : value.toFixed(2))
const process = computed<Detail[]>(() => [
  { label: 'Version', value: wire.value.version }, { label: 'Uptime', value: cell.duration(wire.value.uptimeMs) },
  { label: 'CPU', value: cell.percent(wire.value.cpuPercent) }, { label: 'Resident memory', value: cell.bytes(wire.value.residentMemoryBytes) },
  { label: 'Virtual memory', value: cell.bytes(wire.value.virtualMemoryBytes) }, entry('Threads', wire.value.threadCount),
  entry('Open file descriptors', wire.value.openFileDescriptors), { label: 'Administrative state', value: wire.value.administrativeState },
])
const traffic = computed<Detail[]>(() => [
  entry('Open connections', wire.value.activeConnections), entry('Connection limit', wire.value.maxConnections),
  entry('Accepted', wire.value.acceptedConnections), entry('Rejected', wire.value.rejectedConnections),
  { label: 'Received', value: cell.bytes(wire.value.bytesReceived) }, { label: 'Sent', value: cell.bytes(wire.value.bytesSent) },
  entry('Retry attempts', wire.value.retryAttempts),
])
const inflight = computed<Detail[]>(() => [
  entry('Upstream requests', wire.value.inflight.upstreamRequests),
  { label: 'Oldest upstream request', value: cell.duration(wire.value.inflight.oldestUpstreamAgeMs) },
  entry('Overdue', wire.value.inflight.overdue), entry('Stuck', wire.value.inflight.stuck),
])
const host = computed<Detail[]>(() => [
  { label: 'Load 1m', value: load(wire.value.host.loadAverage1m) }, { label: 'Load 5m', value: load(wire.value.host.loadAverage5m) },
  { label: 'Load 15m', value: load(wire.value.host.loadAverage15m) },
  { label: 'Memory total', value: cell.bytes(wire.value.host.totalMemoryBytes) },
  { label: 'Memory available', value: cell.bytes(wire.value.host.availableMemoryBytes) },
])
</script>

<template lang="pug">
section.ox-panel#process(v-if="model" aria-labelledby="ox-process-title")
  h2#ox-process-title Process
  p.ox-badges
    StatusBadge(:status="wire.state")
    span.ox-sub(v-if="wire.stateReason") {{ wire.stateReason }}
  DetailList(:items="process")
  h3 Connections and traffic
  DetailList(:items="traffic")
  h3 In flight
  DetailList(:items="inflight")
  h3 Host
  p.ox-badges
    StatusBadge(:status="wire.host.state")
  DetailList(:items="host")
</template>
