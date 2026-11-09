<script setup vapor lang="ts">
import { useStatsStore } from '../useStatsStore'
import StatusBadge from './StatusBadge.vue'
import './panels.css'

// Configuration warnings of the running generation, as the server words them.
const { model } = useStatsStore()
</script>

<template lang="pug">
section.ox-panel#warnings(v-if="model" aria-labelledby="ox-warnings-title")
  h2#ox-warnings-title Warnings
  p.ox-muted(v-if="model.warnings.length === 0") No configuration warnings.
  ul.ox-warnings(v-else)
    li(v-for="item in model.warnings" :key="item.key")
      StatusBadge(status="degraded" label="WARNING")
      code {{ item.wire.code }}
      span {{ item.wire.message }}
</template>
