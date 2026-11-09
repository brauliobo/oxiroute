<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredRow } from '../derive'
import type { ServerRow } from '../model'
import { useStatsStore } from '../useStatsStore'
import type { Detail } from './present'
import * as cell from './present'
import DetailList from './DetailList.vue'
import Histogram from './Histogram.vue'
import Latency from './Latency.vue'
import Sparkline from './Sparkline.vue'
import './table.css'

// Everything the snapshot says about one server that the row does not show.
const props = defineProps<{ server: MeasuredRow<ServerRow> }>()
const { model, series } = useStatsStore()
const wire = computed(() => props.server.wire)
const sampledAt = computed(() => model.value!.sampledAtUnixMs)
const bounds = computed(() => model.value!.bucketBoundsMs.operation)
const entry = (label: string, value: bigint | number | undefined): Detail => ({ label, value: cell.exact(value) })
const flag = (label: string, value: boolean): Detail => ({ label, value: cell.yesNo(value) })
const timed = (label: string, unixMs?: number): Detail =>
  ({ label, value: unixMs === undefined ? undefined : `${cell.timestamp(unixMs)} (${cell.since(sampledAt.value, unixMs)})` })

const identity = computed<Detail[]>(() => [
  { label: 'Address', value: wire.value.address },
  { label: 'Origin', value: wire.value.origin },
  { label: 'Status', value: props.server.statusLabel },
  { label: 'Administrative state', value: wire.value.administrativeState },
  { label: 'Health override', value: wire.value.healthOverride },
  { label: 'Health', value: wire.value.health },
  flag('Available', wire.value.available),
  { label: 'Weight', value: wire.value.weight },
  { label: 'Configured weight', value: wire.value.configuredWeight },
  entry('Max connections', wire.value.maxConnections),
  entry('Configured max connections', wire.value.configuredMaxConnections),
  { label: 'Downtime', value: cell.duration(wire.value.downtimeMs) },
  timed('Last status change', wire.value.lastTransitionAtUnixMs),
])
const check = computed<Detail[]>(() => {
  const { check: stats } = wire.value
  return [
    flag('Enabled', stats.enabled), flag('Running', stats.running),
    entry('Successful checks', stats.successful), entry('Failed checks', stats.failed),
    entry('Consecutive successes', stats.consecutiveSuccesses), entry('Consecutive failures', stats.consecutiveFailures),
    timed('Last check', stats.lastCheckedAtUnixMs), { label: 'Last failure', value: stats.lastFailure },
  ]
})
const passive = computed<Detail[]>(() => {
  const stats = wire.value.passive
  return [
    flag('Ejected', stats.ejected), { label: 'Ejection reason', value: stats.ejectionReason },
    timed('Ejected at', stats.ejectedAtUnixMs), timed('Ejected until', stats.ejectionUntilUnixMs),
    entry('Ejections', stats.ejectionCount), entry('Failures', stats.failureCount),
    entry('Consecutive failures', stats.consecutiveFailures), entry('Recoveries', stats.recoveryCount),
    timed('Last recovery', stats.lastRecoveryAtUnixMs),
  ]
})
const attempts = computed<Detail[]>(() => {
  const stats = wire.value.traffic.attempts
  return [
    entry('Succeeded', stats.ok), entry('Connect failed', stats.connectFailed), entry('Timed out', stats.timeout),
    entry('Protocol error', stats.protocolError), entry('Status retry', stats.statusRetry), entry('Rescued', stats.rescued),
    entry('Client closed', stats.clientClosed), entry('Evacuated', stats.evacuated),
  ]
})
const evacuations = computed<Detail[]>(() => {
  const stats = wire.value.traffic.evacuated
  return [entry('Redispatched', stats.redispatched), entry('Failed', stats.failed), entry('Forced', stats.forced),
    entry('Left running', stats.leftRunning)]
})
const responses = computed<Detail[]>(() => {
  const stats = wire.value.traffic.responses
  return [entry('1xx', stats['1xx']), entry('2xx', stats['2xx']), entry('3xx', stats['3xx']), entry('4xx', stats['4xx']),
    entry('5xx', stats['5xx']), entry('No response', stats.none)]
})
const key = (metric: string) => `${props.server.key}#${metric}`
</script>

<template lang="pug">
.ox-detail-grid
  section
    h4 Server
    DetailList(:items="identity")
  section
    h4 Health check
    DetailList(:items="check")
  section
    h4 Passive health
    DetailList(:items="passive")
  section
    h4 Attempts by outcome
    DetailList(:items="attempts")
    h4 Evacuations
    DetailList(:items="evacuations")
  section
    h4 Responses by class
    DetailList(:items="responses")
  section
    h4 Trends
    p.ox-trend
      span Requests/s
      Sparkline(:series="series(key('requests'))" label="Requests per second" :format="cell.rate")
    p.ox-trend
      span Errors/s
      Sparkline(:series="series(key('errors'))" label="Errors per second" :format="cell.rate")
    p.ox-trend
      span Open sessions
      Sparkline(:series="series(key('active'))" label="Open sessions")
  section
    h4 Latency
    Latency(:summary="server.latency.request")
    Histogram(label="Request duration" :histogram="wire.latency.request" :bounds="bounds")
    Histogram(label="Time to first header" :histogram="wire.latency.header" :bounds="bounds")
  p.ox-muted.ox-note Not reported by this build: backup role, peak sessions, connect time, health check duration and history.
</template>
