<script setup vapor lang="ts">
import StatsDashboard from './components/StatsDashboard.vue'
import StatsStatus from './StatsStatus.vue'
import { provideStatsStore } from './useStatsStore'

provideStatsStore()
</script>

<template lang="pug">
main.stats-shell
  h1 OxiRoute statistics
  StatsStatus
  StatsDashboard
</template>
