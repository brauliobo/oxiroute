<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredPool } from '../derive'
import { useStatsStore } from '../useStatsStore'
import DetailList from './DetailList.vue'
import Histogram from './Histogram.vue'
import Latency from './Latency.vue'
import type { Detail } from './present'
import * as cell from './present'
import Sparkline from './Sparkline.vue'
import './table.css'

// Pool selection, queue, in-flight work and canary of one pool, plus its latency histograms.
const props = defineProps<{ pool: MeasuredPool }>()
const { model, series } = useStatsStore()
const wire = computed(() => props.pool.wire)
const sampledAt = computed(() => model.value!.sampledAtUnixMs)
const operation = computed(() => model.value!.bucketBoundsMs.operation)
const queueWait = computed(() => model.value!.bucketBoundsMs.queueWait)
const entry = (label: string, value: bigint | number | undefined): Detail => ({ label, value: cell.exact(value) })

const selection = computed<Detail[]>(() => [
  { label: 'Algorithm', value: wire.value.algorithm },
  { label: 'Available servers', value: `${wire.value.availableServers} of ${wire.value.totalServers}` },
  entry('Selections with no server available', wire.value.unavailableSelections),
  entry('Fail-open selections', wire.value.failOpenSelections),
  entry('In-flight requests', wire.value.inflight.requests),
  { label: 'Oldest in-flight request', value: cell.duration(wire.value.inflight.oldestAgeMs) },
])
const queue = computed<Detail[]>(() => {
  const stats = wire.value.queue
  return [
    entry('Waiting now', stats.current), { label: 'Oldest wait', value: cell.duration(stats.oldestWaitMs) },
    { label: 'Wait limit', value: cell.duration(stats.timeoutMs) }, entry('Queued in total', stats.total),
    entry('Timed out', stats.timeouts), entry('Cancelled', stats.cancellations),
    ...(stats.byClass ?? []).map((item) => entry(`Waiting, class ${item.class}`, item.count)),
  ]
})
const canary = computed<Detail[]>(() => {
  const stats = wire.value.canary
  if (!stats) return []
  return [
    { label: 'Result', value: stats.result }, { label: 'Server', value: stats.server }, { label: 'Reason', value: stats.reason },
    { label: 'Duration', value: cell.duration(stats.durationMs) }, { label: 'Interval', value: cell.duration(stats.intervalMs) },
    entry('Consecutive failures', stats.consecutiveFailures),
    { label: 'Last attempt', value: cell.since(sampledAt.value, stats.lastAttemptUnixMs) },
    { label: 'Last success', value: cell.since(sampledAt.value, stats.lastSuccessUnixMs) },
  ]
})
const key = (metric: string) => `${props.pool.key}#${metric}`
</script>

<template lang="pug">
.ox-detail-grid
  section
    h4 Selection
    DetailList(:items="selection")
  section
    h4 Queue
    DetailList(:items="queue")
    Histogram(label="Queue wait" :histogram="wire.queue.wait" :bounds="queueWait")
  section(v-if="canary.length")
    h4 Canary
    DetailList(:items="canary")
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
    Latency(:summary="pool.latency.request")
    Histogram(label="Request duration" :histogram="wire.latency.request" :bounds="operation")
    Histogram(label="Time to first header" :histogram="wire.latency.header" :bounds="operation")
</template>
