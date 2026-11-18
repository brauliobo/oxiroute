<template lang="pug">
section.monitoring-overview(aria-labelledby="monitoring-heading")
  .section-heading.monitoring-heading
    div
      p.eyebrow Live infrastructure
      h2#monitoring-heading Monitoring overview
    p.snapshot-time Snapshot {{ formatTime(monitoring.sampledAtUnixMs) }} / {{ formatSampleAge(currentUnixMs, monitoring.sampledAtUnixMs) }}

  .monitor-grid
    article.monitor-panel.traffic-panel
      header.panel-heading
        div
          p.eyebrow Aggregate network
          h3 Traffic
        span.panel-index 01
      .traffic-primary
        div
          strong.metric-display {{ formatCount(monitoring.traffic.activeConnections) }}
          span.metric-caption Connections active now
        .connection-total
          span.label Lifetime accepted
          strong {{ formatCount(monitoring.traffic.acceptedConnections) }}
      .direction-grid
        .direction-card
          span.direction-arrow(aria-hidden="true") IN
          div
            span.label Inbound
            strong {{ formatBytes(monitoring.traffic.bytesReceived) }}
        .direction-card
          span.direction-arrow.outbound(aria-hidden="true") OUT
          div
            span.label Outbound
            strong {{ formatBytes(monitoring.traffic.bytesSent) }}

    article.monitor-panel.host-panel
      header.panel-heading
        div
          p.eyebrow Host pressure
          h3 Load / memory
        span.panel-index 02
      .load-list(aria-label="Host load averages")
        .load-row(v-for="load in hostLoads" :key="load.label")
          span.load-window {{ load.label }}
          .load-track(aria-hidden="true")
            span(v-css-vars="{ '--fill': loadFillPercent(load.value) }")
          strong {{ formatDecimal(load.value) }}
      .memory-readout
        .memory-copy
          span.label Memory used
          strong {{ formatOptionalBytes(usedMemoryBytes(monitoring.host)) }} / {{ formatOptionalBytes(monitoring.host.totalMemoryBytes) }}
        .memory-track(aria-hidden="true")
          span(v-css-vars="{ '--fill': memoryUsagePercent(monitoring.host) ?? 0 }")
        span.available-copy {{ formatOptionalBytes(monitoring.host.availableMemoryBytes) }} available

    article.monitor-panel.process-panel
      header.panel-heading
        div
          p.eyebrow OxiRoute process
          h3 Runtime
        span.panel-index 03
      .process-primary
        span.label CPU utilization
        strong.metric-display {{ formatOptionalPercent(monitoring.process.cpuPercent) }}
      .process-grid
        .compact-metric
          span.label Resident
          strong {{ formatOptionalBytes(monitoring.process.residentMemoryBytes) }}
        .compact-metric
          span.label Virtual
          strong {{ formatOptionalBytes(monitoring.process.virtualMemoryBytes) }}
        .compact-metric
          span.label Threads
          strong {{ formatOptionalCount(monitoring.process.threadCount) }}
        .compact-metric
          span.label Open files
          strong {{ formatOptionalCount(monitoring.process.openFileDescriptors) }}

    article.monitor-panel.rtmp-panel
      header.panel-heading
        div
          p.eyebrow Media plane
          h3 RTMP pulse
        span.panel-index 04
      .rtmp-signal
        span.signal-ring(aria-hidden="true")
        div
          strong.metric-display {{ formatCount(monitoring.rtmp.activeStreams) }}
          span.metric-caption Active {{ monitoring.rtmp.activeStreams === 1 ? 'stream' : 'streams' }}
      .rtmp-grid
        .compact-metric
          span.label Publishers
          strong {{ formatCount(monitoring.rtmp.publishers) }}
        .compact-metric
          span.label Subscribers
          strong {{ formatCount(monitoring.rtmp.subscribers) }}
        .compact-metric.payload-metric
          span.label Active media
          strong {{ formatBytes(monitoring.rtmp.mediaPayloadBytesReceived) }}

  section.listener-section(aria-labelledby="listener-heading")
    .listener-heading
      div
        p.eyebrow Bound surfaces
        h3#listener-heading Listeners
      span.listener-count {{ monitoring.listeners.length }} {{ monitoring.listeners.length === 1 ? 'endpoint' : 'endpoints' }}
    p.listener-empty(v-if="monitoring.listeners.length === 0") No listeners are currently bound.
    .listener-list(v-else)
      article.listener-row(
        v-for="listener in monitoring.listeners"
        :key="`${listener.protocol}:${listener.name}:${listener.bind}`"
        :class="`listener-${listener.state}`"
      )
        header.listener-identity
          span.protocol-badge(:class="`protocol-${listener.protocol}`") {{ listenerProtocolLabels[listener.protocol] }}
          div
            h4 {{ listener.name }}
            code {{ listener.bind }}
          span.listener-state(:class="`listener-state-${listener.state}`") {{ listenerStateLabels[listener.state] }}
        .listener-metrics
          .listener-metric
            span.label Active / limit
            strong {{ formatCount(listener.activeConnections) }} / {{ formatLimit(listener.maxConnections) }}
          .listener-metric
            span.label Accepted
            strong {{ formatCount(listener.acceptedConnections) }}
          .listener-metric
            span.label Received
            strong {{ formatBytes(listener.bytesReceived) }}
          .listener-metric
            span.label Sent
            strong {{ formatBytes(listener.bytesSent) }}

  section.pool-section(aria-labelledby="pool-heading")
    .listener-heading
      div
        p.eyebrow Origin readiness
        h3#pool-heading Upstream pools
      span.listener-count {{ monitoring.upstreamPools.length }} {{ monitoring.upstreamPools.length === 1 ? 'pool' : 'pools' }}
    p.listener-empty(v-if="monitoring.upstreamPools.length === 0") No upstream pools are configured.
    .pool-grid(v-else)
      article.pool-card(v-for="pool in monitoring.upstreamPools" :key="pool.name")
        header.pool-heading
          div
            h4 {{ pool.name }}
            span.pool-algorithm Routing: {{ poolAlgorithmLabels[pool.algorithm] }}
          .pool-summary
            strong {{ poolAvailabilityLabel(pool) }}
            span {{ pool.availableEndpoints }} / {{ pool.totalEndpoints }} endpoints available
            span {{ formatCount(pool.unavailableSelections) }} unavailable selections
        ul.endpoint-list(:aria-label="`${pool.name} endpoints`")
          li.endpoint-row(v-for="endpoint in pool.endpoints" :key="endpoint.address")
            code {{ endpoint.address }}
            span.health-state(:class="`health-${endpoint.state}`") {{ endpointHealthLabels[endpoint.state] }}
            span.endpoint-leases Active connections: {{ formatCount(endpoint.activeConnections) }}
            span.endpoint-observation {{ endpoint.lastCheckedAtUnixMs === null ? 'No checks completed' : `Last checked ${formatSampleAge(currentUnixMs, endpoint.lastCheckedAtUnixMs)}` }}
            span.endpoint-checks Total checks: {{ formatCount(endpoint.successfulChecks) }} passed / {{ formatCount(endpoint.failedChecks) }} failed
            span.endpoint-streak Consecutive checks: {{ formatCount(endpoint.consecutiveSuccesses) }} passed / {{ formatCount(endpoint.consecutiveFailures) }} failed
            span.endpoint-failure(v-if="endpoint.lastFailure") Last failure: {{ healthFailureLabels[endpoint.lastFailure] }}
</template>

<script setup lang="ts" vapor>
import { computed } from 'vue'

import type { MonitoringPool, MonitoringSnapshot } from '../api'
import { vCssVars } from '#design/directives/cssVars'
import { formatBytes, formatClockTime as formatTime, formatCount } from '../formatters'
import { listenerStateLabels } from '../runtimeStates'
import { endpointHealthLabels, healthFailureLabels, listenerProtocolLabels, poolAlgorithmLabels } from './labels'
import {
  formatDecimal,
  formatLimit,
  formatOptionalBytes,
  formatOptionalCount,
  formatOptionalPercent,
  formatSampleAge,
  memoryUsagePercent,
  usedMemoryBytes,
} from './telemetry'

const props = defineProps<{
  monitoring: MonitoringSnapshot
  currentUnixMs: number
}>()

const hostLoads = computed(() => [
  { label: '01m', value: props.monitoring.host.loadAverage1m },
  { label: '05m', value: props.monitoring.host.loadAverage5m },
  { label: '15m', value: props.monitoring.host.loadAverage15m },
])
const maximumHostLoad = computed(() => Math.max(
  1,
  ...hostLoads.value.flatMap(({ value }) => value === null ? [] : [value]),
))

function poolAvailabilityLabel(pool: MonitoringPool): string {
  if (pool.availableEndpoints === 0) return 'Unavailable'
  if (pool.availableEndpoints === pool.totalEndpoints) return 'Fully available'
  return 'Degraded'
}

function loadFillPercent(value: number | null): number {
  if (value === null || value <= 0) return 0
  return Math.max(4, (value / maximumHostLoad.value) * 100)
}
</script>

<style scoped src="./base.css"></style>
<style scoped src="./MonitoringOverview.css"></style>
