<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredRow } from '../derive'
import type { ServerRow } from '../model'
import { useStatsStore } from '../useStatsStore'
import { useStatusChanges } from './changes'
import Latency from './Latency.vue'
import * as cell from './present'
import ResponseBar from './ResponseBar.vue'
import ServerDetails from './ServerDetails.vue'
import Sparkline from './Sparkline.vue'
import StatusBadge from './StatusBadge.vue'
import './table.css'

// One server: its row and, when opened, a row with everything else the snapshot holds. A `tbody` per server keeps the
// pair together and lets the table have any number of them.
const props = defineProps<{ server: MeasuredRow<ServerRow> }>()
const { view, model, series, toggleExpanded } = useStatsStore()
const changes = useStatusChanges()
const wire = computed(() => props.server.wire)
const open = computed(() => view.value.expanded.includes(props.server.key))
const flagged = computed(() => changes.changed(props.server.key))
const detailsId = computed(() => `details-${cell.anchorId(props.server.key)}`)
const sampledAt = computed(() => model.value!.sampledAtUnixMs)
const notes = computed(() => [
  wire.value.administrativeState === 'ready' ? '' : `admin ${wire.value.administrativeState}`,
  wire.value.healthOverride === 'auto' ? '' : `forced ${wire.value.healthOverride}`,
].filter(Boolean).join(', '))
const retries = computed(() => wire.value.traffic.attempts.statusRetry + wire.value.traffic.attempts.rescued)
const checks = computed(() => (wire.value.check.enabled
  ? `${cell.exact(wire.value.check.successful)} ok / ${cell.exact(wire.value.check.failed)} failed` : 'checks off'))
const toggle = () => toggleExpanded(props.server.key)
</script>

<template lang="pug">
tbody.ox-row(:class="{ 'ox-changed': flagged, 'ox-open': open }" :data-row="server.key")
  tr
    th.ox-name(scope="row" data-label="Server")
      button.ox-toggle(type="button" :aria-expanded="open" :aria-controls="detailsId" @click="toggle")
        span.ox-chevron(aria-hidden="true")
        span.ox-label {{ server.name }}
      span.ox-sub {{ wire.origin }}
      span.ox-changed-tag(v-if="flagged") status changed
    td(data-label="Status")
      StatusBadge(:status="wire.status" :label="server.statusLabel")
      span.ox-sub(v-if="notes") {{ notes }}
    td(data-label="Sessions")
      span(:title="`${cell.exact(wire.traffic.sessions.current)} open`") {{ cell.sessions(wire.traffic.sessions.current, wire.maxConnections) }}
      span.ox-sub(:title="cell.exact(wire.traffic.sessions.total)") {{ cell.count(wire.traffic.sessions.total) }} total
    td(data-label="Req/s")
      span {{ cell.rate(server.rates.requests) }}
      Sparkline(:series="series(`${server.key}#requests`)" label="Requests per second" :format="cell.rate")
    td(data-label="In")
      span(:title="cell.exact(wire.traffic.bytesReceived)") {{ cell.bytes(wire.traffic.bytesReceived) }}
      span.ox-sub {{ cell.byteRate(server.rates.bytesReceived) }}
    td(data-label="Out")
      span(:title="cell.exact(wire.traffic.bytesSent)") {{ cell.bytes(wire.traffic.bytesSent) }}
      span.ox-sub {{ cell.byteRate(server.rates.bytesSent) }}
    td(data-label="Responses")
      ResponseBar(:responses="wire.traffic.responses")
    td(data-label="Errors")
      span(:title="cell.exact(server.counters.errors)") {{ cell.count(server.counters.errors) }}
      span.ox-sub {{ cell.rate(server.rates.errors) }}
      span.ox-sub(:title="cell.exact(retries)") {{ cell.count(retries) }} retries
    td(data-label="Check")
      span {{ wire.check.enabled ? wire.health : 'unchecked' }}
      span.ox-sub checked {{ cell.since(sampledAt, wire.check.lastCheckedAtUnixMs) }}
      span.ox-sub {{ checks }}
    td(data-label="Weight")
      span {{ wire.weight }}
      span.ox-sub(v-if="wire.configuredWeight !== wire.weight") configured {{ wire.configuredWeight }}
    td(data-label="Downtime")
      span {{ cell.duration(wire.downtimeMs) }}
      span.ox-sub changed {{ cell.since(sampledAt, wire.lastTransitionAtUnixMs) }}
    td(data-label="Latency")
      Latency(:summary="server.latency.request")
  tr.ox-details(v-if="open" :id="detailsId")
    td(colspan="12")
      ServerDetails(:server="server")
</template>
