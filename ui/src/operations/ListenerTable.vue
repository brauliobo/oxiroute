<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Table from '#design/table/Table.vue'
import type { AdministrativeState, MonitoringListener } from '../api'
import { formatCount } from '../formatters'
import type { AdmissionCommand } from './commands'
import { ADMIN_BADGES, LISTENER_BADGES, limitText } from './labels'
import StateButtons from './StateButtons.vue'
import './page.css'

const COLUMNS = ['Listener', 'State', 'Connections', 'Admission']

defineProps<{ listeners: readonly MonitoringListener[]; disabled: boolean }>()
const emit = defineEmits<{ command: [command: AdmissionCommand] }>()

const change = (listener: MonitoringListener, state: AdministrativeState) =>
  emit('command', { kind: 'listener-state', listener, state })
</script>

<template lang="pug">
EmptyState(v-if="listeners.length === 0" title="No listeners") No listener inventory is available.
Table(v-else caption="Listeners: state, connections and administrative controls" :columns="COLUMNS")
  tbody
    tr(v-for="listener in listeners" :key="listener.name" :data-listener="listener.name")
      th.ox-name(scope="row" data-label="Listener")
        | {{ listener.name }}
        code.ox-ops-sub {{ listener.bind }}
      td(data-label="State")
        .ox-ops-badges
          Badge(:tone="ADMIN_BADGES[listener.administrativeState].tone" :label="ADMIN_BADGES[listener.administrativeState].label")
          Badge(:tone="LISTENER_BADGES[listener.state].tone" :label="LISTENER_BADGES[listener.state].label")
      td(data-label="Connections") {{ formatCount(listener.activeConnections) }} active of {{ limitText(listener.maxConnections) }}, {{ formatCount(listener.acceptedConnections) }} accepted
      td(data-label="Admission")
        StateButtons(
          :current="listener.administrativeState"
          :subject="`listener ${listener.name}`"
          :disabled="disabled"
          @change="(state) => change(listener, state)"
        )
</template>
