<script setup vapor lang="ts">
import { computed } from 'vue'
import { useStatsStore } from '../useStatsStore'
import { LISTENER_COLUMNS } from './columns'
import ListenerRows from './ListenerRows.vue'
import SortHeader from './SortHeader.vue'
import './table.css'

const { model, visible } = useStatsStore()
const listeners = computed(() => visible.value?.listeners ?? [])
const empty = computed(() => (model.value?.listeners.length ? 'No listeners match the current filters.' : 'No listeners are configured.'))
</script>

<template lang="pug">
section.ox-section#listeners(aria-labelledby="ox-listeners-title")
  h2#ox-listeners-title Listeners
  p.ox-muted(v-if="listeners.length === 0") {{ empty }}
  table.ox-table.ox-listeners(v-else)
    caption.ox-sr-only Listeners: state, connections, rates, bytes, errors and latency
    thead
      tr
        SortHeader(v-for="item in LISTENER_COLUMNS" :key="item.label" :label="item.label" :column="item.column")
    ListenerRows(v-for="listener in listeners" :key="listener.key" :listener="listener")
</template>
