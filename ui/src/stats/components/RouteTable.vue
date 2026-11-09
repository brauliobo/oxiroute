<script setup vapor lang="ts">
import { useStatsStore } from '../useStatsStore'
import { rowKey } from '../model'
import * as cell from './present'
import './table.css'

// Routes: which listeners, host and path select them and where they send the request.
const { model } = useStatsStore()
const COLUMNS = ['Route', 'Listeners', 'Host', 'Path', 'Methods', 'Action', 'Target', 'Retries', 'Pool selections', 'Rescues']
const poolLink = (pool: string) => `#${cell.anchorId(rowKey('pool', pool))}`
</script>

<template lang="pug">
section.ox-section#routes(aria-labelledby="ox-routes-title")
  h2#ox-routes-title Routes
  p.ox-muted(v-if="model && model.routes.length === 0") No routes are configured.
  table.ox-table.ox-routes-table(v-else-if="model")
    caption.ox-sr-only Routes: listeners, host and path selectors, methods, action, target pool and counters
    thead
      tr
        th(v-for="label in COLUMNS" :key="label" scope="col") {{ label }}
    tbody.ox-row(v-for="route in model.routes" :key="route.key" :data-row="route.key")
      tr
        th.ox-name(scope="row" data-label="Route") {{ route.name }}
        td(data-label="Listeners") {{ route.wire.listeners.join(', ') }}
        td(data-label="Host")
          span {{ route.wire.host?.value ?? 'any' }}
          span.ox-sub(v-if="route.wire.host") {{ cell.words(route.wire.host.kind) }}
        td(data-label="Path")
          span {{ route.wire.path.value }}
          span.ox-sub {{ cell.words(route.wire.path.kind) }}
        td(data-label="Methods") {{ route.wire.methods.join(', ') || 'any' }}
        td(data-label="Action") {{ cell.words(route.wire.action) }}
        td(data-label="Target")
          a(v-if="route.wire.pool" :href="poolLink(route.wire.pool)") {{ route.wire.pool }}
          template(v-else-if="route.wire.status !== undefined") status {{ route.wire.status }}
          template(v-else) {{ cell.text() }}
        td(data-label="Retries") {{ cell.text(route.wire.maxRetries) }}
        td(data-label="Pool selections")
          span(v-for="item in route.wire.poolSelections ?? []" :key="item.pool" :title="cell.exact(item.count)") {{ item.pool }} {{ cell.count(item.count) }}
          template(v-if="!route.wire.poolSelections") {{ cell.text() }}
        td(data-label="Rescues")
          span(v-for="item in route.wire.rescues ?? []" :key="item.status" :title="cell.exact(item.count)") {{ item.status }}: {{ cell.count(item.count) }}
          template(v-if="!route.wire.rescues") {{ cell.text() }}
</template>
