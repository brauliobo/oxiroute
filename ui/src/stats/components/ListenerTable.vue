<script setup vapor lang="ts">
import { computed } from 'vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Table from '#design/table/Table.vue'
import { useStatsStore } from '../useStatsStore'
import ColumnHeads from './ColumnHeads.vue'
import { LISTENER_COLUMNS } from './columns'
import ListenerRows from './ListenerRows.vue'
import './table.css'

const { model, visible } = useStatsStore()
const listeners = computed(() => visible.value?.listeners ?? [])
const empty = computed(() => (model.value?.listeners.length ? 'No listeners match the current filters.' : 'No listeners are configured.'))
</script>

<template lang="pug">
section.ox-section#listeners(aria-labelledby="ox-listeners-title")
  h2#ox-listeners-title Listeners
  EmptyState(v-if="listeners.length === 0") {{ empty }}
  Table.ox-listeners(v-else caption="Listeners: state, connections, rates, bytes, errors and latency")
    template(v-slot:head)
      ColumnHeads(:columns="LISTENER_COLUMNS")
    ListenerRows(v-for="listener in listeners" :key="listener.key" :listener="listener")
</template>
