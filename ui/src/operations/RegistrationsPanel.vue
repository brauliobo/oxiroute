<script setup vapor lang="ts">
import { onMounted, ref } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import { formatDuration } from '#design/format'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import {
  ApiError,
  deregisterPoolServer,
  setRegisteredServerState,
  type AdministrativeState,
  type RegistrationStatus,
} from '../api'
import { confirmAction } from '../shell/confirm'
import { ADMIN_BADGES, humanize, limitText, type StateBadge } from './labels'
import { useRegistrations } from './useRegistrations'
import './page.css'

const COLUMNS = ['Server', 'Address', 'Registry status', 'State', 'Lease', 'Weight and limit', 'Labels', 'Actions']
const STATUS_BADGES: Record<RegistrationStatus, StateBadge> = {
  active: { label: 'Active', tone: 'ok' },
  shadowed: { label: 'Shadowed', tone: 'warn' },
  orphaned: { label: 'Orphaned', tone: 'bad' },
}

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const { list, error, disabled, loading, load } = useRegistrations(() => props.token, () => emit('unauthorized'))
const acting = ref(false)
const actionError = ref<string | null>(null)
const actionNotice = ref<string | null>(null)

const labelText = (labels: Record<string, string>) =>
  Object.entries(labels).map(([key, value]) => `${key}=${value}`).join(', ') || 'none'

async function updateState(pool: string, name: string, state: AdministrativeState): Promise<void> {
  acting.value = true
  actionError.value = null
  actionNotice.value = null
  try {
    await setRegisteredServerState(pool, name, state, props.token)
    actionNotice.value = `Updated registered server ${name} in pool ${pool} to ${state} (registry state only; config file unchanged).`
    await load()
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      emit('unauthorized')
      return
    }
    actionError.value = err instanceof Error ? err.message : 'Unable to update registered server state'
  } finally {
    acting.value = false
  }
}

async function removeRegistration(pool: string, name: string): Promise<void> {
  const ok = await confirmAction({
    title: `Deregister ${name} from pool ${pool}?`,
    message: 'Removes this dynamic server from the registry immediately. Configuration files are not modified.',
    confirmLabel: 'Deregister server',
    danger: true,
  })
  if (!ok) return
  acting.value = true
  actionError.value = null
  actionNotice.value = null
  try {
    await deregisterPoolServer(pool, name, props.token)
    actionNotice.value = `Deregistered ${name} from pool ${pool}.`
    await load()
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      emit('unauthorized')
      return
    }
    actionError.value = err instanceof Error ? err.message : 'Unable to deregister server'
  } finally {
    acting.value = false
  }
}

onMounted(load)
</script>

<template lang="pug">
Panel.ox-ops-registrations(title="Registrations")
  .ox-ops-buttons
    Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing...' : 'Refresh registrations' }}
  Notice(kind="alert")
    Banner(v-if="error || actionError" tone="bad")
      strong Registrations unavailable.
      |  {{ error || actionError }}
  Notice(kind="status")
    Banner(v-if="actionNotice" tone="ok")
      strong {{ actionNotice }}
  Loading(v-if="loading && !list && !disabled" label="Reading registrations")
  EmptyState(v-if="disabled" title="Server registration is not configured") No pool accepts registrations, so there are no dynamic servers to list.
  template(v-if="list")
    Notice(kind="status")
      Banner(v-if="list.degraded" tone="warn")
        strong The registry is degraded.
        |  {{ list.degraded }}
    p.ox-muted {{ list.registrations.length }} registered {{ list.registrations.length === 1 ? 'server' : 'servers' }}. {{ list.writer ? 'This process owns the registry.' : 'This process only reads the registry.' }}
    EmptyState(v-if="list.registrations.length === 0" title="No registrations") No machine has registered itself with a pool yet.
    Table(v-else caption="Registered servers with lease time left and status" :columns="COLUMNS")
      tbody
        tr(v-for="entry in list.registrations" :key="`${entry.registration.pool}:${entry.registration.name}`")
          th.ox-name(scope="row" data-label="Server")
            | {{ entry.registration.name }}
            span.ox-ops-sub {{ entry.registration.pool }}
          td(data-label="Address") {{ entry.registration.address }}
          td(data-label="Registry status")
            Badge(:tone="STATUS_BADGES[entry.status].tone" :label="STATUS_BADGES[entry.status].label")
          td(data-label="State")
            Badge(:tone="ADMIN_BADGES[entry.registration.effectiveState].tone" :label="ADMIN_BADGES[entry.registration.effectiveState].label")
            span.ox-ops-sub announced {{ humanize(entry.registration.announcedState) }}, operator {{ entry.registration.operatorState }}
          td(data-label="Lease") {{ entry.expiresInMs === null ? 'no lease' : `expires in ${formatDuration(entry.expiresInMs)}` }}
          td(data-label="Weight and limit") weight {{ entry.registration.weight }}, priority {{ entry.registration.priority }}, limit {{ limitText(entry.registration.maxConnections) }}
          td(data-label="Labels") {{ labelText(entry.registration.labels) }}
          td(data-label="Actions")
            .ox-ops-buttons(v-if="list.writer")
              Button(small :disabled="acting" @click="updateState(entry.registration.pool, entry.registration.name, 'ready')") Ready
              Button(small :disabled="acting" @click="updateState(entry.registration.pool, entry.registration.name, 'drain')") Drain
              Button(small :disabled="acting" @click="updateState(entry.registration.pool, entry.registration.name, 'maintenance')") Maintenance
              Button(small variant="danger" :disabled="acting" @click="removeRegistration(entry.registration.pool, entry.registration.name)") Deregister
            span.ox-muted(v-else) Read-only
</template>
