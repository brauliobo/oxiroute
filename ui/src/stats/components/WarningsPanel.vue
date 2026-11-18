<script setup vapor lang="ts">
import Panel from '#design/panel/Panel.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import { useStatsStore } from '../useStatsStore'
import StatusBadge from './StatusBadge.vue'
import './dashboard.css'

// Configuration warnings of the running generation, as the server words them.
const { model } = useStatsStore()
</script>

<template lang="pug">
Panel#warnings(v-if="model" title="Warnings")
  EmptyState(v-if="model.warnings.length === 0") No configuration warnings.
  ul.ox-warnings(v-else)
    li(v-for="item in model.warnings" :key="item.key")
      StatusBadge(status="degraded" label="WARNING")
      code {{ item.wire.code }}
      span {{ item.wire.message }}
</template>
