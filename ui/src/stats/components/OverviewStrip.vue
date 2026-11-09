<script setup vapor lang="ts">
import { computed } from 'vue'
import { useStatsStore } from '../useStatsStore'
import { summarizeOverview } from './overview'
import * as cell from './present'
import StatusBadge from './StatusBadge.vue'
import './widgets.css'

// The page at a glance: what is running, how much traffic it carries, and what needs attention.
const { model } = useStatsStore()
const overview = computed(() => model.value && summarizeOverview(model.value))
</script>

<template lang="pug">
section.ox-overview(v-if="overview" aria-label="Overview")
  dl.ox-tiles
    .ox-tile
      dt Version
      dd {{ overview.version }}
      dd.ox-sub up {{ cell.duration(overview.uptimeMs) }}
    .ox-tile
      dt Generation
      dd {{ overview.generation.number }}
      dd.ox-sub {{ overview.generation.accepting ? 'accepting' : 'not accepting' }}{{ overview.generation.degraded ? ', degraded' : '' }}
      dd.ox-sub(v-if="overview.generation.activeRevision") revision {{ overview.generation.activeRevision }}
    .ox-tile
      dt Connections
      dd {{ cell.sessions(overview.connections.active, overview.connections.max) }}
      dd.ox-sub(:title="cell.exact(overview.connections.accepted)") {{ cell.count(overview.connections.accepted) }} accepted
    .ox-tile
      dt Requests
      dd {{ cell.rate(overview.requestRate) }}
      dd.ox-sub errors {{ cell.percent(overview.errorPercent) }}
    .ox-tile
      dt Traffic
      dd in {{ cell.byteRate(overview.bytesInRate) }}
      dd out {{ cell.byteRate(overview.bytesOutRate) }}
    .ox-tile
      dt Pools
      dd {{ overview.pools.up }} of {{ overview.pools.total }} up
      dd.ox-sub {{ overview.pools.degraded }} degraded, {{ overview.pools.down }} down
    .ox-tile
      dt Servers
      dd {{ overview.servers.up }} of {{ overview.servers.total }} up
      dd.ox-sub listeners {{ overview.listeners.up }} of {{ overview.listeners.total }} ok
    .ox-tile
      dt Certificates
      template(v-if="overview.certificate")
        dd
          StatusBadge(:status="overview.certificate.severity" :label="cell.certificateLabel(overview.certificate.severity)")
        dd.ox-sub {{ overview.certificate.name }} {{ cell.remaining(overview.certificate.remainingMs) }}
      dd(v-else) {{ cell.text() }}
    .ox-tile
      dt Warnings
      dd
        a(href="#warnings") {{ overview.warnings }}
</template>
