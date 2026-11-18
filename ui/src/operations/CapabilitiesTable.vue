<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import type { RuntimeStatus } from '../api'
import { humanize, type StateBadge } from './labels'
import './page.css'

const COLUMNS = ['Capability', 'Status', 'Listeners', 'Details']
const STATUS_BADGES: Record<'active' | 'unconfigured' | 'blocked', StateBadge> = {
  active: { label: 'Active', tone: 'ok' },
  unconfigured: { label: 'Not configured', tone: 'neutral' },
  blocked: { label: 'Blocked', tone: 'bad' },
}

interface Row {
  name: string
  badge: StateBadge
  listeners: string
  details: string
}

const props = defineProps<{ capabilities: RuntimeStatus['capabilities'] }>()

const names = (listeners: readonly string[]) => (listeners.length ? listeners.join(', ') : 'none')
const blocked = (reason: string | null) => (reason ? ` Blocked: ${humanize(reason)}.` : '')
const rows = computed<Row[]>(() => {
  const { supervision, udp, http3 } = props.capabilities
  return [
    {
      name: 'Supervision',
      badge: { label: supervision.mode === 'supervised' ? 'Supervised' : 'Direct', tone: 'info' },
      listeners: 'not applicable',
      details: `Descriptor adoption ${humanize(supervision.descriptorAdoption.status)} (manifest ${supervision.descriptorAdoption.manifestVersion}).`,
    },
    {
      name: 'UDP relay',
      badge: STATUS_BADGES[udp.status],
      listeners: names(udp.listeners),
      details: `${udp.supported ? 'Supported' : 'Not supported'}; graceful drain.${blocked(udp.blockedReason)}`,
    },
    ...([['HTTP/3 reverse', http3.reverse], ['HTTP/3 forward', http3.forward]] as const).map(([name, h3]) => ({
      name,
      badge: STATUS_BADGES[h3.status],
      listeners: names(h3.listeners),
      details: `${h3.supported ? 'Supported' : 'Not supported'}; ${h3.alpn.join(', ')}, TLS ${h3.tlsMinVersion} minimum, no 0-RTT, no migration.${blocked(h3.blockedReason)}`,
    })),
  ]
})
</script>

<template lang="pug">
Panel.ox-ops-capabilities(title="Capabilities")
  Table(caption="Transport capabilities and supervision mode" :columns="COLUMNS")
    tbody
      tr(v-for="row in rows" :key="row.name")
        th.ox-name(scope="row" data-label="Capability") {{ row.name }}
        td(data-label="Status")
          Badge(:tone="row.badge.tone" :label="row.badge.label")
        td(data-label="Listeners") {{ row.listeners }}
        td(data-label="Details") {{ row.details }}
</template>
