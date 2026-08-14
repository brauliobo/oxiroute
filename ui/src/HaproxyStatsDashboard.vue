<template lang="pug">
section.haproxy-stats(aria-labelledby="stats-heading")
  header.stats-header
    div
      p.eyebrow HAProxy-compatible telemetry
      h2#stats-heading Statistics
      p.stats-deck Process, listener, backend, and server counters from the active runtime generation.
    p.snapshot-time Snapshot {{ formatTime(monitoring.sampledAtUnixMs) }} / {{ formatTelemetryAge(monitoring.sampledAtUnixMs) }}

  .stats-kpis
    article.stats-kpi(v-for="metric in headlineMetrics" :key="metric.label")
      span.label {{ metric.label }}
      strong(:class="{ mono: metric.mono }") {{ metric.value }}

  .stats-grid
    article.stats-panel
      header.panel-heading
        div
          p.eyebrow Process
          h3 Runtime admission
        span.panel-index 01
      dl.metric-list
        div(v-for="metric in processMetrics" :key="metric.label")
          dt {{ metric.label }}
          dd(:class="{ mono: metric.mono }") {{ metric.value }}

    article.stats-panel
      header.panel-heading
        div
          p.eyebrow Aggregate traffic
          h3 Front-door totals
        span.panel-index 02
      dl.metric-list
        div(v-for="metric in trafficMetrics" :key="metric.label")
          dt {{ metric.label }}
          dd(:class="{ mono: metric.mono }") {{ metric.value }}

    article.stats-panel
      header.panel-heading
        div
          p.eyebrow Host pressure
          h3 System sample
        span.panel-index 03
      dl.metric-list
        div(v-for="metric in hostMetrics" :key="metric.label")
          dt {{ metric.label }}
          dd(:class="{ mono: metric.mono }") {{ metric.value }}

  section.stats-section(aria-labelledby="listener-stats-heading")
    header.section-heading
      div
        p.eyebrow Frontends / listeners
        h3#listener-stats-heading Bound surfaces
      span.section-count {{ monitoring.listeners.length }} {{ monitoring.listeners.length === 1 ? 'surface' : 'surfaces' }}
    p.empty-state(v-if="monitoring.listeners.length === 0") No listeners are currently bound.
    .table-scroll(v-else)
      table.stats-table
        caption Active listener counters
        thead
          tr
            th(scope="col") Surface
            th(scope="col") State
            th(scope="col") Active / limit
            th(scope="col") Accepted
            th(scope="col") Rejected
            th(scope="col") In
            th(scope="col") Out
        tbody
          tr(v-for="listener in monitoring.listeners" :key="`${listener.protocol}:${listener.name}:${listener.bind}`")
            th(scope="row")
              strong {{ listener.name }}
              small {{ listenerProtocolLabel(listener.protocol) }} / {{ listener.bind }}
            td
              span.status-pill(:class="`status-${listener.state}`") {{ humanize(listener.state) }}
            td.mono {{ formatCount(listener.activeConnections) }} / {{ formatLimit(listener.maxConnections) }}
            td.mono {{ formatCount(listener.acceptedConnections) }}
            td.mono {{ formatCount(listener.rejectedConnections) }}
            td.mono {{ formatBytes(listener.bytesReceived) }}
            td.mono {{ formatBytes(listener.bytesSent) }}

  section.stats-section(aria-labelledby="pool-stats-heading")
    header.section-heading
      div
        p.eyebrow Backends / servers
        h3#pool-stats-heading Upstream health and queues
      span.section-count {{ monitoring.upstreamPools.length }} {{ monitoring.upstreamPools.length === 1 ? 'backend' : 'backends' }}
    p.empty-state(v-if="monitoring.upstreamPools.length === 0") No upstream pools are configured.
    .pool-list(v-else)
      article.pool-panel(v-for="pool in monitoring.upstreamPools" :key="pool.name")
        header.pool-heading
          div
            p.eyebrow Backend
            h4 {{ pool.name }}
            p.pool-algorithm {{ humanize(pool.algorithm) }} balancing
          span.status-pill(:class="`status-${poolStatus(pool)}`") {{ humanize(poolStatus(pool)) }}
        dl.pool-metrics
          div
            dt Available servers
            dd {{ pool.availableEndpoints }} / {{ pool.totalEndpoints }}
          div
            dt Queued now
            dd.mono {{ formatCount(pool.queued) }}
          div
            dt Queued total
            dd.mono {{ formatCount(pool.queuedTotal) }}
          div
            dt Queue timeouts
            dd.mono {{ formatCount(pool.queueTimeouts) }}
          div
            dt Queue cancellations
            dd.mono {{ formatCount(pool.queueCancellations) }}
          div
            dt Unavailable selections
            dd.mono {{ formatCount(pool.unavailableSelections) }}
        .table-scroll
          table.stats-table.server-table
            caption {{ pool.name }} server counters
            thead
              tr
                th(scope="col") Server
                th(scope="col") Health
                th(scope="col") Admin / checks
                th(scope="col") Active / limit
                th(scope="col") Checks
                th(scope="col") Observation
                th(scope="col") Override
            tbody
              tr(v-for="endpoint in pool.endpoints" :key="`${pool.name}:${endpoint.name}:${endpoint.address}`")
                th(scope="row")
                  strong {{ endpoint.name }}
                  small {{ endpoint.address }}
                td
                  span.status-pill(:class="`status-${endpoint.state}`") {{ humanize(endpoint.state) }}
                td
                  span {{ humanize(endpoint.administrativeState) }}
                  small {{ endpoint.checksEnabled ? 'Enabled' : 'Disabled' }} / {{ endpoint.checksRunning ? 'running' : 'stopped' }}
                td.mono {{ formatCount(endpoint.activeConnections) }} / {{ formatLimit(endpoint.maxConnections) }}
                td
                  span.mono {{ formatCount(endpoint.successfulChecks) }} / {{ formatCount(endpoint.failedChecks) }}
                  small {{ formatCount(endpoint.consecutiveSuccesses) }} pass / {{ formatCount(endpoint.consecutiveFailures) }} fail in streak
                td
                  span {{ endpoint.lastCheckedAtUnixMs === null ? 'No checks completed' : formatTelemetryAge(endpoint.lastCheckedAtUnixMs) }}
                  small Transition {{ endpoint.lastTransitionAtUnixMs === null ? '--' : formatTelemetryAge(endpoint.lastTransitionAtUnixMs) }}
                td
                  span {{ humanize(endpoint.healthOverride) }}
                  small Configured {{ formatLimit(endpoint.configuredMaxConnections) }}
                tr.endpoint-detail(v-if="endpoint.lastFailure" :key="`${pool.name}:${endpoint.name}:failure`")
                  td(colspan="7") Last failure: {{ humanize(endpoint.lastFailure) }}
</template>

<script setup lang="ts">
import { computed } from 'vue'

import {
  formatBytes,
  formatClockTime as formatTime,
  formatCount,
  formatTelemetryAge,
  formatTelemetryDuration,
} from './formatters'
import type { MonitoringListenerProtocol, MonitoringPool, MonitoringSnapshot } from './api'

const props = defineProps<{ monitoring: MonitoringSnapshot }>()

type Metric = { label: string; value: string; mono?: boolean }

const monitoring = computed(() => props.monitoring)
const headlineMetrics = computed<Metric[]>(() => [
  { label: 'Active connections', value: formatCount(monitoring.value.traffic.activeConnections), mono: true },
  { label: 'Accepted', value: formatCount(monitoring.value.traffic.acceptedConnections), mono: true },
  { label: 'Rejected', value: formatCount(monitoring.value.traffic.rejectedConnections), mono: true },
  {
    label: 'Traffic moved',
    value: formatBytes(addDecimal(monitoring.value.traffic.bytesReceived, monitoring.value.traffic.bytesSent)),
    mono: true,
  },
])
const processMetrics = computed<Metric[]>(() => [
  { label: 'Administrative state', value: humanize(monitoring.value.process.administrativeState) },
  { label: 'Active connections', value: formatCount(monitoring.value.process.activeConnections), mono: true },
  { label: 'Connection limit', value: formatLimit(monitoring.value.process.maxConnections), mono: true },
  { label: 'Rejected connections', value: formatCount(monitoring.value.process.rejectedConnections), mono: true },
  { label: 'Retry attempts', value: formatCount(monitoring.value.process.retryAttempts), mono: true },
  { label: 'CPU utilization', value: formatPercent(monitoring.value.process.cpuPercent) },
  { label: 'Uptime', value: formatTelemetryDuration(monitoring.value.uptimeMs), mono: true },
  { label: 'Resident memory', value: formatOptionalBytes(monitoring.value.process.residentMemoryBytes), mono: true },
  { label: 'Virtual memory', value: formatOptionalBytes(monitoring.value.process.virtualMemoryBytes), mono: true },
  { label: 'Threads', value: formatOptionalCount(monitoring.value.process.threadCount), mono: true },
  { label: 'Open files', value: formatOptionalCount(monitoring.value.process.openFileDescriptors), mono: true },
])
const trafficMetrics = computed<Metric[]>(() => [
  { label: 'Active connections', value: formatCount(monitoring.value.traffic.activeConnections), mono: true },
  { label: 'Accepted connections', value: formatCount(monitoring.value.traffic.acceptedConnections), mono: true },
  { label: 'Rejected connections', value: formatCount(monitoring.value.traffic.rejectedConnections), mono: true },
  { label: 'Bytes received', value: formatBytes(monitoring.value.traffic.bytesReceived), mono: true },
  { label: 'Bytes sent', value: formatBytes(monitoring.value.traffic.bytesSent), mono: true },
])
const hostMetrics = computed<Metric[]>(() => [
  { label: 'Load average / 1m', value: formatOptionalDecimal(monitoring.value.host.loadAverage1m), mono: true },
  { label: 'Load average / 5m', value: formatOptionalDecimal(monitoring.value.host.loadAverage5m), mono: true },
  { label: 'Load average / 15m', value: formatOptionalDecimal(monitoring.value.host.loadAverage15m), mono: true },
  { label: 'Total memory', value: formatOptionalBytes(monitoring.value.host.totalMemoryBytes), mono: true },
  { label: 'Available memory', value: formatOptionalBytes(monitoring.value.host.availableMemoryBytes), mono: true },
  { label: 'Used memory', value: formatOptionalBytes(usedMemoryBytes(monitoring.value)), mono: true },
])

function addDecimal(left: string, right: string): string {
  return (BigInt(left) + BigInt(right)).toString()
}

function usedMemoryBytes(snapshot: MonitoringSnapshot): number | null {
  const { totalMemoryBytes, availableMemoryBytes } = snapshot.host
  return totalMemoryBytes === null || availableMemoryBytes === null
    ? null
    : Math.max(0, totalMemoryBytes - availableMemoryBytes)
}

function formatLimit(value: number | null): string {
  return value === null ? 'Unbounded' : formatCount(value)
}

function formatPercent(value: number | null): string {
  return value === null ? 'Unavailable' : `${value.toFixed(1).replace(/\.0$/, '')}%`
}

function formatOptionalBytes(value: number | null): string {
  return value === null ? 'Unavailable' : formatBytes(value)
}

function formatOptionalCount(value: number | null): string {
  return value === null ? 'Unavailable' : formatCount(value)
}

function formatOptionalDecimal(value: number | null): string {
  return value === null ? 'Unavailable' : value.toFixed(2)
}

function humanize(value: string): string {
  return value.replace(/_/g, ' ').replace(/\b\w/g, (character) => character.toUpperCase())
}

function listenerProtocolLabel(protocol: MonitoringListenerProtocol): string {
  const labels: Record<MonitoringListenerProtocol, string> = {
    http: 'HTTP',
    tcp: 'TCP',
    rtmp: 'RTMP',
    http3: 'HTTP/3',
    udp: 'UDP',
    forward_http1: 'Forward H1',
    forward_http2: 'Forward H2',
    forward_http3: 'Forward H3',
  }
  return labels[protocol]
}

function poolStatus(pool: MonitoringPool): string {
  if (pool.availableEndpoints === 0) return 'unavailable'
  if (pool.availableEndpoints === pool.totalEndpoints) return 'available'
  return 'degraded'
}
</script>

<style scoped>
.haproxy-stats {
  padding: clamp(42px, 7vw, 74px) 0;
}

.stats-header,
.section-heading,
.panel-heading,
.pool-heading {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 18px;
}

.stats-header {
  margin-bottom: 24px;
}

.stats-header h2,
.section-heading h3,
.stats-panel h3,
.pool-heading h4 {
  margin: 4px 0 0;
  font-family: Georgia, "Times New Roman", serif;
  font-weight: 400;
  letter-spacing: -0.04em;
}

.stats-header h2 {
  font-size: clamp(2.2rem, 5vw, 4rem);
}

.stats-header .eyebrow,
.stats-deck {
  margin-bottom: 0;
}

.stats-deck {
  max-width: 620px;
  margin-top: 10px;
  color: #8f9788;
}

.snapshot-time,
.section-count,
.pool-algorithm {
  color: #7f8777;
  font-size: 0.78rem;
}

.stats-kpis {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  margin-bottom: 14px;
  border: 1px solid #3a4034;
  background: #10130e;
}

.stats-kpi {
  display: grid;
  gap: 10px;
  min-width: 0;
  padding: 20px;
}

.stats-kpi + .stats-kpi {
  border-left: 1px solid #34392f;
}

.stats-kpi strong {
  overflow: hidden;
  color: #b6ff51;
  font-family: Georgia, "Times New Roman", serif;
  font-size: clamp(1.5rem, 3vw, 2.35rem);
  font-weight: 400;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 14px;
}

.stats-panel,
.pool-panel,
.empty-state {
  border: 1px solid #3a4034;
  background: rgb(23 26 21 / 90%);
}

.stats-panel {
  min-width: 0;
  padding: clamp(18px, 2.2vw, 28px);
}

.panel-index {
  color: #4c5347;
  font-family: Georgia, "Times New Roman", serif;
  font-size: 2.2rem;
}

.metric-list,
.pool-metrics {
  display: grid;
  grid-template-columns: 1fr 1fr;
  margin: 30px 0 0;
  border-top: 1px solid #34392f;
}

.metric-list > div,
.pool-metrics > div {
  display: grid;
  gap: 7px;
  min-width: 0;
  padding: 13px 12px 13px 0;
}

.metric-list > div:nth-child(even),
.pool-metrics > div:nth-child(even) {
  padding-left: 12px;
  border-left: 1px solid #34392f;
}

.metric-list > div:nth-child(n + 3),
.pool-metrics > div:nth-child(n + 3) {
  border-top: 1px solid #34392f;
}

dt,
.label {
  color: #929a88;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.67rem;
  font-weight: 650;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

dd {
  margin: 0;
  color: #eef2e7;
  font-size: 0.84rem;
}

.stats-section {
  margin-top: 42px;
}

.section-heading {
  margin-bottom: 16px;
}

.section-heading h3 {
  font-size: clamp(1.8rem, 4vw, 2.7rem);
}

.table-scroll {
  overflow-x: auto;
  border: 1px solid #3a4034;
  background: #10130e;
}

.stats-table {
  width: 100%;
  min-width: 880px;
  border-collapse: collapse;
  text-align: left;
}

.stats-table caption {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip: rect(0 0 0 0);
}

.stats-table th,
.stats-table td {
  padding: 14px 16px;
  border-bottom: 1px solid #292e26;
  vertical-align: top;
  white-space: nowrap;
}

.stats-table thead th {
  color: #929a88;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.63rem;
  font-weight: 650;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}

.stats-table tbody th {
  min-width: 200px;
  font-weight: 500;
}

.stats-table tbody tr:last-child > * {
  border-bottom: 0;
}

.stats-table strong,
.stats-table small {
  display: block;
}

.stats-table small {
  max-width: 260px;
  margin-top: 5px;
  overflow: hidden;
  color: #7f8777;
  font-size: 0.7rem;
  font-weight: 400;
  overflow-wrap: anywhere;
  text-overflow: ellipsis;
  white-space: normal;
}

.status-pill {
  display: inline-block;
  min-width: 76px;
  padding: 5px 8px;
  border: 1px solid #5f6a56;
  color: #c5d1b8;
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  font-size: 0.61rem;
  text-align: center;
  text-transform: uppercase;
}

.status-listening,
.status-healthy,
.status-available {
  border-color: #607d4c;
  color: #b6ff51;
}

.status-failed,
.status-stopped,
.status-unhealthy,
.status-unavailable {
  border-color: #81483f;
  color: #ff8b78;
}

.status-degraded,
.status-unknown {
  border-color: #806f47;
  color: #ffcf70;
}

.pool-list {
  display: grid;
  gap: 14px;
}

.pool-panel {
  min-width: 0;
  padding: clamp(18px, 2.2vw, 28px);
}

.pool-heading {
  align-items: flex-start;
}

.pool-heading h4 {
  font-size: 1.8rem;
}

.pool-algorithm {
  margin: 7px 0 0;
}

.pool-metrics {
  margin: 20px 0;
}

.server-table {
  min-width: 1120px;
}

.endpoint-detail td {
  border-bottom: 1px solid #292e26;
  color: #ff8b78;
  font-size: 0.74rem;
}

.empty-state {
  margin: 0;
  padding: 24px;
  color: #8f9788;
}

.mono {
  font-family: "IBM Plex Mono", "SFMono-Regular", Consolas, monospace !important;
}

@media (max-width: 1050px) {
  .stats-grid {
    grid-template-columns: 1fr 1fr;
  }

  .stats-panel:last-child {
    grid-column: 1 / -1;
  }
}

@media (max-width: 720px) {
  .stats-header,
  .section-heading,
  .pool-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .stats-kpis,
  .stats-grid {
    grid-template-columns: 1fr 1fr;
  }

  .stats-panel:last-child {
    grid-column: auto;
  }
}

@media (max-width: 480px) {
  .stats-kpis,
  .stats-grid,
  .metric-list,
  .pool-metrics {
    grid-template-columns: 1fr;
  }

  .stats-kpi + .stats-kpi,
  .metric-list > div:nth-child(even),
  .pool-metrics > div:nth-child(even) {
    padding-left: 0;
    border-left: 0;
  }

  .stats-kpi + .stats-kpi,
  .metric-list > div:nth-child(n + 2),
  .pool-metrics > div:nth-child(n + 2) {
    border-top: 1px solid #34392f;
  }

  .stats-panel:last-child {
    grid-column: auto;
  }
}
</style>
