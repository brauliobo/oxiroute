<script setup vapor lang="ts">
import { computed } from 'vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Table from '#design/table/Table.vue'
import { useStatsStore } from '../useStatsStore'
import ColumnHeads from './ColumnHeads.vue'
import { SERVER_COLUMNS } from './columns'
import PoolHead from './PoolHead.vue'
import ServerRows from './ServerRows.vue'
import './table.css'

const { model, visible, view } = useStatsStore()
const pools = computed(() => visible.value?.pools ?? [])
const folded = (key: string) => view.value.collapsed.includes(key)
const empty = computed(() => (model.value?.pools.length ? 'No pools or servers match the current filters.' : 'No pools are configured.'))
</script>

<template lang="pug">
section.ox-section#pools(aria-labelledby="ox-pools-title")
  h2#ox-pools-title Pools and servers
  EmptyState(v-if="pools.length === 0") {{ empty }}
  Table.ox-servers(
    v-else
    caption="Pools with their servers: status, sessions, rates, bytes, responses, errors, checks, weight, downtime and latency"
  )
    template(v-slot:head)
      ColumnHeads(:columns="SERVER_COLUMNS")
    template(v-for="pool in pools" :key="pool.key")
      PoolHead(:pool="pool")
      template(v-if="!folded(pool.key)")
        ServerRows(v-for="server in pool.servers" :key="server.key" :server="server")
</template>
