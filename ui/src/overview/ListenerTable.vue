<script setup vapor lang="ts">
import StatusBadge from '#design/status/StatusBadge.vue'
import Table from '#design/table/Table.vue'
import type { MonitoringListener } from '../api'
import { formatBytes, formatCount } from '../formatters'
import { administrativeStateLabels, listenerProtocolLabels } from './labels'
import { formatLimit } from './telemetry'
import './styles.css'

defineProps<{ listeners: readonly MonitoringListener[] }>()

const COLUMNS = ['Listener', 'State', 'Connections', 'Accepted', 'Rejected', 'Received', 'Sent']
const rowKey = ({ protocol, name, bind }: MonitoringListener) => `${protocol}:${name}:${bind}`
</script>

<template lang="pug">
Table(caption="Listener counters" :columns="COLUMNS")
  tbody.ox-row(v-for="listener in listeners" :key="rowKey(listener)")
    tr
      th.ox-name(scope="row" data-label="Listener")
        | {{ listener.name }}
        span.ox-cell-note {{ listenerProtocolLabels[listener.protocol] }} / {{ listener.bind }}
      td(data-label="State")
        StatusBadge(:status="listener.state")
        span.ox-cell-note(v-if="listener.administrativeState !== 'ready'") {{ administrativeStateLabels[listener.administrativeState] }} by operator
      td(data-label="Connections") {{ formatCount(listener.activeConnections) }} / {{ formatLimit(listener.maxConnections) }}
      td(data-label="Accepted") {{ formatCount(listener.acceptedConnections) }}
      td(data-label="Rejected") {{ formatCount(listener.rejectedConnections) }}
      td(data-label="Received") {{ formatBytes(listener.bytesReceived) }}
      td(data-label="Sent") {{ formatBytes(listener.bytesSent) }}
</template>
