<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredRow } from '../derive'
import { rowKey } from '../model'
import type { ListenerRow } from '../model'
import { useStatsStore } from '../useStatsStore'
import DetailList from './DetailList.vue'
import Histogram from './Histogram.vue'
import Latency from './Latency.vue'
import type { Detail } from './present'
import * as cell from './present'
import Sparkline from './Sparkline.vue'
import './table.css'

// The rest of a listener's snapshot (bind, TLS, outcomes, proxy protocol, cache) and the routes that name it.
const props = defineProps<{ listener: MeasuredRow<ListenerRow> }>()
const { model, series } = useStatsStore()
const wire = computed(() => props.listener.wire)
const bounds = computed(() => model.value!.bucketBoundsMs.operation)
const routes = computed(() => model.value!.routes.filter((route) => route.wire.listeners.includes(props.listener.name)))
const outcomes = computed(() => wire.value.http?.outcomes ?? wire.value.tcpRelays?.outcomes ?? [])
const entry = (label: string, value: bigint | number | undefined): Detail => ({ label, value: cell.exact(value) })
const identity = computed<Detail[]>(() => [
  { label: 'Bind', value: wire.value.bind }, { label: 'Protocol', value: wire.value.protocol },
  { label: 'Service', value: wire.value.service }, { label: 'TLS profile', value: wire.value.tlsProfile },
  { label: 'Upstream pool', value: wire.value.upstreamPool }, { label: 'Administrative state', value: wire.value.administrativeState },
  entry('Open connections', wire.value.activeConnections), entry('Connection limit', wire.value.maxConnections),
  entry('Accepted', wire.value.acceptedConnections), entry('Rejected', wire.value.rejectedConnections),
  entry('Requests', props.listener.counters.requests),
])
const results = computed<Detail[]>(() => outcomes.value.map((item) => entry(item.result, item.count)))
const proxyProtocol = computed<Detail[]>(() => (wire.value.proxyProtocol?.outcomes ?? []).map((item) => entry(item.result, item.count)))
const cache = computed<Detail[]>(() => {
  const stats = wire.value.cache
  return stats ? [entry('Hits', stats.hits), entry('Misses', stats.misses), entry('Admissions', stats.admissions),
    entry('Evictions', stats.evictions)] : []
})
const poolLink = (pool: string) => `#${cell.anchorId(rowKey('pool', pool))}`
const key = (metric: string) => `${props.listener.key}#${metric}`
</script>

<template lang="pug">
.ox-detail-grid
  section
    h4 Listener
    DetailList(:items="identity")
  section(v-if="results.length")
    h4 Outcomes
    DetailList(:items="results")
  section(v-if="proxyProtocol.length")
    h4 PROXY protocol
    DetailList(:items="proxyProtocol")
  section(v-if="cache.length")
    h4 Cache
    DetailList(:items="cache")
  section
    h4 Routes
    p.ox-muted(v-if="routes.length === 0") No route names this listener.
    ul.ox-routes(v-else)
      li(v-for="route in routes" :key="route.key")
        span.ox-label {{ route.name }}
        |  {{ route.wire.host?.value ?? 'any host' }}{{ route.wire.path.value }}
        template(v-if="route.wire.pool")
          |  to 
          a(:href="poolLink(route.wire.pool)") {{ route.wire.pool }}
        template(v-else)
          |  {{ cell.words(route.wire.action) }}
  section
    h4 Trends
    p.ox-trend
      span Requests/s
      Sparkline(:series="series(key('requests'))" label="Requests per second" :format="cell.rate")
    p.ox-trend
      span Errors/s
      Sparkline(:series="series(key('errors'))" label="Errors per second" :format="cell.rate")
    p.ox-trend
      span Open connections
      Sparkline(:series="series(key('active'))" label="Open connections")
  section(v-if="listener.histograms.request")
    h4 Latency
    Latency(:summary="listener.latency.request")
    Histogram(label="Request duration" :histogram="listener.histograms.request" :bounds="bounds")
</template>
