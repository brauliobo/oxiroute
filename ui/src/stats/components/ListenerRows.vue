<script setup vapor lang="ts">
import { computed } from 'vue'
import type { MeasuredRow } from '../derive'
import type { ListenerRow } from '../model'
import { useStatsStore } from '../useStatsStore'
import { useStatusChanges } from './changes'
import Latency from './Latency.vue'
import ListenerDetails from './ListenerDetails.vue'
import * as cell from './present'
import Sparkline from './Sparkline.vue'
import StatusBadge from './StatusBadge.vue'
import './table.css'

// One listener: its row and, when opened, a row with the rest of its snapshot and the routes it serves.
const props = defineProps<{ listener: MeasuredRow<ListenerRow> }>()
const { view, series, toggleExpanded } = useStatsStore()
const changes = useStatusChanges()
const wire = computed(() => props.listener.wire)
const open = computed(() => view.value.expanded.includes(props.listener.key))
const flagged = computed(() => changes.changed(props.listener.key))
const detailsId = computed(() => `details-${cell.anchorId(props.listener.key)}`)
const kind = computed(() => [wire.value.protocol, wire.value.service].filter(Boolean).join(' '))
const note = computed(() => (wire.value.administrativeState === 'ready' ? '' : `admin ${wire.value.administrativeState}`))
</script>

<template lang="pug">
tbody.ox-row(:class="{ 'ox-changed': flagged, 'ox-open': open }" :data-row="listener.key")
  tr
    th.ox-name(scope="row" data-label="Listener")
      button.ox-toggle(type="button" :aria-expanded="open" :aria-controls="detailsId" @click="toggleExpanded(listener.key)")
        span.ox-chevron(aria-hidden="true")
        span.ox-label {{ listener.name }}
      span.ox-sub {{ kind }}
      span.ox-changed-tag(v-if="flagged") state changed
    td(data-label="State")
      StatusBadge(:status="wire.state")
      span.ox-sub(v-if="note") {{ note }}
    td(data-label="Connections")
      span(:title="`${cell.exact(wire.activeConnections)} open`") {{ cell.sessions(wire.activeConnections, wire.maxConnections) }}
      span.ox-sub(:title="cell.exact(wire.acceptedConnections)") {{ cell.count(wire.acceptedConnections) }} accepted
      span.ox-sub(:title="cell.exact(wire.rejectedConnections)") {{ cell.count(wire.rejectedConnections) }} rejected
    td(data-label="Req/s")
      span {{ cell.rate(listener.rates.requests) }}
      Sparkline(:series="series(`${listener.key}#requests`)" label="Requests per second" :format="cell.rate")
    td(data-label="In")
      span(:title="cell.exact(wire.bytesReceived)") {{ cell.bytes(wire.bytesReceived) }}
      span.ox-sub {{ cell.byteRate(listener.rates.bytesReceived) }}
    td(data-label="Out")
      span(:title="cell.exact(wire.bytesSent)") {{ cell.bytes(wire.bytesSent) }}
      span.ox-sub {{ cell.byteRate(listener.rates.bytesSent) }}
    td(data-label="Errors")
      span(:title="cell.exact(listener.counters.errors)") {{ cell.count(listener.counters.errors) }}
      span.ox-sub {{ cell.rate(listener.rates.errors) }}
    td(data-label="Latency")
      Latency(:summary="listener.latency.request")
  tr.ox-details(v-if="open" :id="detailsId")
    td(colspan="8")
      ListenerDetails(:listener="listener")
</template>
