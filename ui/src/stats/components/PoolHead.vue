<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredPool } from '../derive'
import { useStatsStore } from '../useStatsStore'
import { useStatusChanges } from './changes'
import Latency from './Latency.vue'
import PoolDetails from './PoolDetails.vue'
import * as cell from './present'
import ResponseBar from './ResponseBar.vue'
import Sparkline from './Sparkline.vue'
import StatusBadge from './StatusBadge.vue'
import './table.css'

// The head of a pool in the server table: its name, status and queue, folding its servers away, and a totals row.
const props = defineProps<{ pool: MeasuredPool }>()
const { view, series, toggleExpanded, toggleCollapsed } = useStatsStore()
const changes = useStatusChanges()
const wire = computed(() => props.pool.wire)
const folded = computed(() => view.value.collapsed.includes(props.pool.key))
const open = computed(() => view.value.expanded.includes(props.pool.key))
const flagged = computed(() => changes.changed(props.pool.key))
const detailsId = computed(() => `details-${cell.anchorId(props.pool.key)}`)
const shown = computed(() => `${props.pool.servers.length} of ${wire.value.totalServers} servers shown`)
const retries = computed(() => wire.value.traffic.attempts.statusRetry + wire.value.traffic.attempts.rescued)
</script>

<template lang="pug">
tbody.ox-pool(:id="cell.anchorId(pool.key)" :class="{ 'ox-changed': flagged }" :data-row="pool.key")
  tr.ox-group
    th(scope="rowgroup" colspan="12")
      .ox-group-head
        button.ox-toggle(type="button" :aria-expanded="!folded" @click="toggleCollapsed(pool.key)")
          span.ox-chevron(aria-hidden="true")
          span.ox-label {{ pool.name }}
        StatusBadge(:status="wire.status")
        span {{ wire.availableServers }} of {{ wire.totalServers }} servers available
        span.ox-sub {{ wire.algorithm }}
        span.ox-sub(:title="`oldest wait ${cell.duration(wire.queue.oldestWaitMs)}`") queue {{ wire.queue.current }}, timeout {{ cell.duration(wire.queue.timeoutMs) }}
        span.ox-sub(v-if="folded") {{ shown }}
        span.ox-changed-tag(v-if="flagged") status changed
        button.ox-more(type="button" :aria-expanded="open" :aria-controls="detailsId" @click="toggleExpanded(pool.key)") Pool details
  tr.ox-total
    th.ox-name(scope="row" data-label="Pool") All servers
    td(data-label="Status")
      StatusBadge(:status="wire.status")
    td(data-label="Sessions")
      span(:title="cell.exact(wire.traffic.sessions.total)") {{ pool.active }}
      span.ox-sub {{ cell.count(wire.traffic.sessions.total) }} total
    td(data-label="Req/s")
      span {{ cell.rate(pool.rates.requests) }}
      Sparkline(:series="series(`${pool.key}#requests`)" label="Requests per second" :format="cell.rate")
    td(data-label="In")
      span(:title="cell.exact(wire.traffic.bytesReceived)") {{ cell.bytes(wire.traffic.bytesReceived) }}
      span.ox-sub {{ cell.byteRate(pool.rates.bytesReceived) }}
    td(data-label="Out")
      span(:title="cell.exact(wire.traffic.bytesSent)") {{ cell.bytes(wire.traffic.bytesSent) }}
      span.ox-sub {{ cell.byteRate(pool.rates.bytesSent) }}
    td(data-label="Responses")
      ResponseBar(:responses="wire.traffic.responses")
    td(data-label="Errors")
      span(:title="cell.exact(pool.counters.errors)") {{ cell.count(pool.counters.errors) }}
      span.ox-sub {{ cell.rate(pool.rates.errors) }}
      span.ox-sub(:title="cell.exact(retries)") {{ cell.count(retries) }} retries
    td(data-label="Check")
      span {{ wire.availableServers }}/{{ wire.totalServers }} available
    td(data-label="Weight") {{ cell.text() }}
    td(data-label="Downtime") {{ cell.text() }}
    td(data-label="Latency")
      Latency(:summary="pool.latency.request")
  tr.ox-details(v-if="open" :id="detailsId")
    td(colspan="12")
      PoolDetails(:pool="pool")
</template>
