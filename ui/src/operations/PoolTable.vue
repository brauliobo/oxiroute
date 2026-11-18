<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Table from '#design/table/Table.vue'
import type { AdministrativeState, MonitoringPool } from '../api'
import { formatCount } from '../formatters'
import type { AdmissionCommand } from './commands'
import { humanize, poolAdminBadge, uniformAdminState } from './labels'
import StateButtons from './StateButtons.vue'
import './page.css'

const COLUMNS = ['Pool', 'Administrative state', 'Servers', 'Admission']

defineProps<{ pools: readonly MonitoringPool[]; disabled: boolean }>()
const emit = defineEmits<{ command: [command: AdmissionCommand] }>()

const change = (pool: MonitoringPool, state: AdministrativeState) => emit('command', { kind: 'pool-state', pool, state })
const evacuate = (pool: MonitoringPool) => emit('command', { kind: 'pool-evacuate', pool })
</script>

<template lang="pug">
EmptyState(v-if="pools.length === 0" title="No pools") No upstream pool inventory is available.
Table(v-else caption="Pools: administrative state, availability and controls" :columns="COLUMNS")
  tbody
    tr(v-for="pool in pools" :key="pool.name" :data-pool="pool.name")
      th.ox-name(scope="row" data-label="Pool")
        | {{ pool.name }}
        span.ox-ops-sub {{ humanize(pool.algorithm) }}
      td(data-label="Administrative state")
        Badge(:tone="poolAdminBadge(pool).tone" :label="poolAdminBadge(pool).label")
      td(data-label="Servers") {{ pool.availableEndpoints }} of {{ pool.totalEndpoints }} available, {{ formatCount(pool.queued) }} queued
      td(data-label="Admission")
        .ox-ops-buttons
          StateButtons(
            :current="uniformAdminState(pool)"
            :subject="`pool ${pool.name}`"
            :disabled="disabled"
            @change="(state) => change(pool, state)"
          )
          Button(small variant="danger" :disabled="disabled" :aria-label="`Evacuate pool ${pool.name}`" @click="evacuate(pool)") Evacuate
</template>
