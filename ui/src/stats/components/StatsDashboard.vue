<script setup vapor lang="ts">
import { provideStatusChanges } from './changes'
import CertificatesPanel from './CertificatesPanel.vue'
import GenerationPanel from './GenerationPanel.vue'
import ListenerTable from './ListenerTable.vue'
import OverviewStrip from './OverviewStrip.vue'
import PoolTable from './PoolTable.vue'
import ProcessPanel from './ProcessPanel.vue'
import RouteTable from './RouteTable.vue'
import WarningsPanel from './WarningsPanel.vue'
import { useStatsStore } from '../useStatsStore'
import './dashboard.css'

// Every data view of the page, in reading order: overview, listeners, routes, pools with servers, then the process,
// generation, certificates and warnings. The store must be provided by an ancestor. Nothing shows before the first snapshot.
const { model } = useStatsStore()
provideStatusChanges()
</script>

<template lang="pug">
.ox-dashboard(v-if="model")
  OverviewStrip
  ListenerTable
  RouteTable
  PoolTable
  .ox-panels
    GenerationPanel
    ProcessPanel
    CertificatesPanel
    WarningsPanel
</template>
