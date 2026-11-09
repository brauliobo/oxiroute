<script setup vapor lang="ts">
import { computed } from 'vue'
import { useStatsStore } from '../useStatsStore'
import { SERVER_COLUMNS } from './columns'
import PoolHead from './PoolHead.vue'
import ServerRows from './ServerRows.vue'
import SortHeader from './SortHeader.vue'
import './table.css'

// Pools with their servers, HAProxy style: one table, a head and totals per pool, one body per server.
const { model, visible, view } = useStatsStore()
const pools = computed(() => visible.value?.pools ?? [])
const folded = (key: string) => view.value.collapsed.includes(key)
const empty = computed(() => (model.value?.pools.length ? 'No pools or servers match the current filters.' : 'No pools are configured.'))
</script>

<template lang="pug">
section.ox-section#pools(aria-labelledby="ox-pools-title")
  h2#ox-pools-title Pools and servers
  p.ox-muted(v-if="pools.length === 0") {{ empty }}
  table.ox-table.ox-servers(v-else)
    caption.ox-sr-only Pools with their servers: status, sessions, rates, bytes, responses, errors, checks, weight, downtime and latency
    thead
      tr
        SortHeader(v-for="item in SERVER_COLUMNS" :key="item.label" :label="item.label" :column="item.column")
    template(v-for="pool in pools" :key="pool.key")
      PoolHead(:pool="pool")
      template(v-if="!folded(pool.key)")
        ServerRows(v-for="server in pool.servers" :key="server.key" :server="server")
</template>
