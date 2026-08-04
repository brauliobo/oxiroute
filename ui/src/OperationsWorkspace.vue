<template lang="pug">
section.operations-workspace(aria-labelledby="operations-heading" :aria-busy="loading || mutating")
  header.workspace-heading
    div
      p.eyebrow Local control plane
      h2#operations-heading Operations
      p.workspace-deck Inspect the active process, generations, listeners, pools, and upstream server health without editing the canonical draft.
    button.secondary-button(type="button" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh operations' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to inspect or change local runtime state.

  .loading-panel(v-else-if="loading && !status && !generation" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Loading operational state
      p Reading the active generation and health inventories.

  p.notice.error-notice.operation-error(v-if="error" role="alert")
    strong Operations unavailable.
    |  {{ error }}
  p.notice.success-notice(v-if="message" role="status" aria-live="polite") {{ message }}

  template(v-if="token && (status || generation || listeners || pools || servers)")
    section.status-section(aria-labelledby="runtime-status-heading")
      .section-heading
        div
          p.eyebrow Operational state
          h3#runtime-status-heading Runtime status
        span.status-badge(:class="statusClass") {{ statusLabel }}
      .status-grid
        article.status-card
          span.label Active revision
          code {{ shortRevision(activeRevision) }}
          small {{ generation?.activeAccepting ? 'Accepting new traffic' : 'Not accepting new traffic' }}
        article.status-card
          span.label Disk revision
          code {{ shortRevision(generation?.diskRevision ?? status?.diskRevision ?? null) }}
          small Canonical file observed by the control plane
        article.status-card
          span.label Candidate / previous
          code {{ shortRevision(generation?.candidateRevision ?? null) }} / {{ shortRevision(generation?.previousRevision ?? status?.previousRevision ?? null) }}
          small Candidate state is not active until publication completes
        article.status-card
          span.label Last failure
          strong(:class="{ 'warning-copy': generation?.lastFailure || status?.degraded }") {{ generation?.lastFailure ?? (status?.degraded ? 'Runtime degraded' : 'None reported') }}
          small {{ generation ? `${generation.activations} activations / ${generation.failures} failures` : 'Generation counters unavailable' }}

    section.generation-panel(aria-labelledby="generation-heading")
      .section-heading
        div
          p.eyebrow Publication control
          h3#generation-heading Generations
        span.panel-note(v-if="generation?.quarantinedRevision") Quarantined {{ shortRevision(generation.quarantinedRevision) }}
      .generation-history
        .generation-record
          span.label Active
          code {{ shortRevision(generation?.activeRevision ?? null) }}
          strong {{ generation?.activeAccepting ? 'Serving' : 'Draining' }}
        .generation-record
          span.label Candidate
          code {{ shortRevision(generation?.candidateRevision ?? null) }}
          strong {{ generation?.candidateRevision ? 'Preparing or waiting' : 'None' }}
        .generation-record
          span.label Previous
          code {{ shortRevision(generation?.previousRevision ?? null) }}
          strong {{ generation?.previousRevision ? 'Rollback target' : 'None available' }}
        .generation-record
          span.label Quarantined
          code {{ shortRevision(generation?.quarantinedRevision ?? null) }}
          strong {{ generation?.quarantinedRevision ? 'Excluded from rollback' : 'None' }}
      .action-row
        button.primary-button(
          type="button"
          data-generation-action="reload"
          :disabled="!canMutate || mutating !== null"
          @click="reload"
        ) Reload persisted configuration
        button.secondary-button(
          type="button"
          data-generation-action="rollback"
          :disabled="!canMutate || mutating !== null || !generation?.previousRevision"
          @click="rollback"
        ) Roll back to previous
        button.danger-button(
          type="button"
          data-generation-action="drain"
          :disabled="!canMutate || mutating !== null"
          @click="drainGenerationNow"
        ) Drain active generation
        button.danger-button(
          type="button"
          data-process-action="drain"
          :disabled="!canMutate || mutating !== null"
          @click="drainProcessNow"
        ) Drain process
        button.shutdown-button(
          type="button"
          data-process-action="shutdown"
          :disabled="!canMutate || mutating !== null"
          @click="shutdown"
        ) Request shutdown

    .inventory-grid
      section.inventory-panel(aria-labelledby="listener-inventory-heading")
        header.panel-heading
          div
            p.eyebrow Admission
            h3#listener-inventory-heading Listeners
          span.panel-note {{ listeners?.listeners.length ?? 0 }}
        p.empty-list(v-if="!listeners || listeners.listeners.length === 0") No listener inventory is available.
        article.inventory-row(v-for="listener in listeners?.listeners ?? []" :key="listener.name")
          .inventory-identity
            strong {{ listener.name }}
            code {{ listener.bind }}
            span.state-chip(:class="`state-${listener.administrativeState}`") {{ humanize(listener.administrativeState) }}
          .inventory-detail
            span {{ listener.state }} / {{ formatCount(listener.activeConnections) }} active
            span {{ formatCount(listener.acceptedConnections) }} accepted
          .action-row.compact-actions
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeListener(listener.name, 'ready')") Ready
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeListener(listener.name, 'drain')") Drain
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeListener(listener.name, 'maintenance')") Maintenance

      section.inventory-panel(aria-labelledby="pool-inventory-heading")
        header.panel-heading
          div
            p.eyebrow Admission
            h3#pool-inventory-heading Pools
          span.panel-note {{ pools?.pools.length ?? 0 }}
        p.empty-list(v-if="!pools || pools.pools.length === 0") No upstream pool inventory is available.
        article.inventory-row(v-for="pool in pools?.pools ?? []" :key="pool.name")
          .inventory-identity
            strong {{ pool.name }}
            code {{ pool.algorithm }}
            span.state-chip(:class="`state-${poolState(pool)}`") {{ poolState(pool) }}
          .inventory-detail
            span {{ pool.availableEndpoints }} / {{ pool.totalEndpoints }} available
            span {{ formatCount(pool.queued) }} queued
          .action-row.compact-actions
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changePool(pool.name, 'ready')") Ready
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changePool(pool.name, 'drain')") Drain
            button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changePool(pool.name, 'maintenance')") Maintenance

      section.inventory-panel.server-panel(aria-labelledby="server-inventory-heading")
        header.panel-heading
          div
            p.eyebrow Endpoint control
            h3#server-inventory-heading Servers
          span.panel-note {{ servers?.servers.length ?? 0 }}
        p.empty-list(v-if="!servers || servers.servers.length === 0") No upstream servers are configured.
        article.server-row(v-for="entry in servers?.servers ?? []" :key="serverKey(entry.pool, entry.server.name)")
          .server-heading
            .inventory-identity
              strong {{ entry.server.name }}
              code {{ entry.pool }} / {{ entry.server.address }}
              span.state-chip(:class="`state-${entry.server.administrativeState}`") {{ humanize(entry.server.administrativeState) }}
            span.health-chip(:class="`health-${entry.server.state}`") {{ humanize(entry.server.state) }}
          .inventory-detail
            span Override: {{ entry.server.healthOverride }}
            span Checks: {{ entry.server.checksEnabled ? 'enabled' : 'disabled' }}
            span Limit: {{ entry.server.maxConnections === null ? 'unbounded' : formatCount(entry.server.maxConnections) }}
            span {{ formatCount(entry.server.successfulChecks) }} passed / {{ formatCount(entry.server.failedChecks) }} failed
          .server-controls
            .action-row.compact-actions
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerState(entry, 'ready')") Ready
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerState(entry, 'drain')") Drain
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerState(entry, 'maintenance')") Maintenance
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerHealth(entry, 'auto')") Auto health
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerHealth(entry, 'up')") Force up
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="changeServerHealth(entry, 'down')") Force down
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="toggleChecks(entry)") {{ entry.server.checksEnabled ? 'Disable checks' : 'Enable checks' }}
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="refreshDns(entry)") Refresh DNS
            form.capacity-form(@submit.prevent="applyCapacity(entry)")
              label(:for="`server-capacity-${serverKey(entry.pool, entry.server.name)}`") Max connections
              input(:id="`server-capacity-${serverKey(entry.pool, entry.server.name)}`" type="number" min="1" step="1" placeholder="unbounded" :value="capacityDraft[serverKey(entry.pool, entry.server.name)] ?? ''" @input="setCapacityDraft(entry, $event)")
              button.small-button(type="submit" :disabled="!canMutate || mutating !== null") Apply limit
              button.small-button(type="button" :disabled="!canMutate || mutating !== null" @click="resetCapacity(entry)") Reset limit
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import {
  ApiError,
  drainGeneration,
  drainProcess,
  fetchGenerations,
  fetchListeners,
  fetchPools,
  fetchServers,
  fetchStatus,
  refreshServerDns,
  reloadGeneration,
  rollbackGeneration,
  setListenerAdministrativeState,
  setPoolAdministrativeState,
  setServerAdministrativeState,
  setServerChecks,
  setServerHealthOverride,
  setServerMaxConnections,
  shutdownProcess,
  type AdministrativeState,
  type GenerationStatus,
  type HealthOverride,
  type ListenerInventoryResponse,
  type MonitoringPool,
  type RuntimeStatus,
  type ServerInventoryEntry,
  type ServerInventoryResponse,
  type PoolInventoryResponse,
} from './api'
import { formatCount } from './formatters'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const status = ref<RuntimeStatus | null>(null)
const generation = ref<GenerationStatus | null>(null)
const listeners = ref<ListenerInventoryResponse | null>(null)
const pools = ref<PoolInventoryResponse | null>(null)
const servers = ref<ServerInventoryResponse | null>(null)
const loading = ref(false)
const mutating = ref<string | null>(null)
const error = ref<string | null>(null)
const message = ref<string | null>(null)
const capacityDraft = ref<Record<string, string>>({})
let controller: AbortController | null = null
let loadRequest: Promise<void> | null = null

const activeRevision = computed(() => generation.value?.activeRevision ?? status.value?.activeRevision ?? null)
const canMutate = computed(() => Boolean(props.token && activeRevision.value))
const statusLabel = computed(() => {
  if (status.value?.degraded || generation.value?.degraded) return 'Degraded'
  if (generation.value?.activeAccepting === false) return 'Draining'
  return 'Ready'
})
const statusClass = computed(() => statusLabel.value.toLowerCase())

async function load(): Promise<void> {
  if (!props.token) return
  if (loadRequest) return loadRequest
  controller?.abort()
  const nextController = new AbortController()
  controller = nextController
  loading.value = true
  error.value = null
  message.value = null
  const request = Promise.all([
    fetchStatus(props.token, nextController.signal),
    fetchGenerations(props.token, nextController.signal),
    fetchListeners(props.token, nextController.signal),
    fetchPools(props.token, nextController.signal),
    fetchServers(props.token, nextController.signal),
  ]).then(([nextStatus, nextGeneration, nextListeners, nextPools, nextServers]) => {
    if (nextController.signal.aborted) return
    status.value = nextStatus
    generation.value = nextGeneration.generation
    listeners.value = nextListeners
    pools.value = nextPools
    servers.value = nextServers
  }).catch((requestError: unknown) => {
    if (nextController.signal.aborted) return
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    error.value = errorMessage(requestError, 'The operations API did not respond.')
  }).finally(() => {
    if (controller === nextController) controller = null
    if (loadRequest === request) loadRequest = null
    loading.value = false
  })
  loadRequest = request
  return request
}

async function runMutation(
  label: string,
  action: (revision: string, token: string) => Promise<unknown>,
  successMessage: string | ((result: unknown) => string) = `${label} applied.`,
): Promise<void> {
  const revision = activeRevision.value
  if (!props.token || !revision || !window.confirm(`Confirm ${label}? This uses active revision ${shortRevision(revision)}.`)) return
  mutating.value = label
  error.value = null
  message.value = null
  try {
    const result = await action(revision, props.token)
    message.value = typeof successMessage === 'function' ? successMessage(result) : successMessage
    await load()
  } catch (requestError) {
    if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
    if (requestError instanceof ApiError && requestError.status === 409) await load()
    error.value = errorMessage(requestError, `${label} failed.`)
  } finally {
    mutating.value = null
  }
}

function reload(): Promise<void> {
  return runMutation('reload the persisted configuration', reloadGeneration)
}

function rollback(): Promise<void> {
  return runMutation('roll back to the previous generation', rollbackGeneration)
}

function drainGenerationNow(): Promise<void> {
  return runMutation('drain the active generation', (revision, token) => drainGeneration(revision, token))
}

function drainProcessNow(): Promise<void> {
  return runMutation('drain the process', drainProcess)
}

function shutdown(): Promise<void> {
  return runMutation('request process shutdown', shutdownProcess, 'Shutdown requested. The process may stop serving this page.')
}

function changeListener(name: string, state: AdministrativeState): Promise<void> {
  return runMutation(`set listener ${name} to ${state}`, (revision, token) =>
    setListenerAdministrativeState([name], state, revision, token),
  )
}

function changePool(name: string, state: AdministrativeState): Promise<void> {
  return runMutation(`set pool ${name} to ${state}`, (revision, token) =>
    setPoolAdministrativeState([name], state, revision, token),
  )
}

function target(entry: ServerInventoryEntry) {
  return [{ pool: entry.pool, server: entry.server.name }]
}

function changeServerState(entry: ServerInventoryEntry, state: AdministrativeState): Promise<void> {
  return runMutation(`set server ${entry.server.name} to ${state}`, (revision, token) =>
    setServerAdministrativeState(target(entry), state, revision, token),
  )
}

function changeServerHealth(entry: ServerInventoryEntry, health: HealthOverride): Promise<void> {
  return runMutation(`set server ${entry.server.name} health to ${health}`, (revision, token) =>
    setServerHealthOverride(target(entry), health, revision, token),
  )
}

function toggleChecks(entry: ServerInventoryEntry): Promise<void> {
  const enabled = !entry.server.checksEnabled
  return runMutation(`${enabled ? 'enable' : 'disable'} checks for ${entry.server.name}`, (revision, token) =>
    setServerChecks(target(entry), enabled, revision, token),
  )
}

async function refreshDns(entry: ServerInventoryEntry): Promise<void> {
  await runMutation('refresh server DNS', (revision, token) => refreshServerDns(target(entry), revision, token), (result) => {
    if (!result || typeof result !== 'object' || !('servers' in result)) return 'DNS refresh completed.'
    const failed = (result.servers as Array<{ outcome: string }>).filter((server) => server.outcome === 'failed').length
    return failed === 0 ? 'DNS refresh completed.' : `DNS refresh completed with ${failed} failed target(s).`
  })
}

function setCapacityDraft(entry: ServerInventoryEntry, event: Event): void {
  capacityDraft.value[serverKey(entry.pool, entry.server.name)] = (event.target as HTMLInputElement).value
}

function applyCapacity(entry: ServerInventoryEntry): Promise<void> {
  const key = serverKey(entry.pool, entry.server.name)
  const raw = capacityDraft.value[key] ?? ''
  const value = raw === '' ? null : Number(raw)
  if (value !== null && (!Number.isSafeInteger(value) || value <= 0)) {
    error.value = 'Maximum connections must be a positive whole number or blank to reset it.'
    return Promise.resolve()
  }
  return runMutation(`set the connection limit for ${entry.server.name}`, (revision, token) =>
    setServerMaxConnections(target(entry), value, revision, token),
  )
}

function resetCapacity(entry: ServerInventoryEntry): Promise<void> {
  capacityDraft.value[serverKey(entry.pool, entry.server.name)] = ''
  return runMutation(`reset the connection limit for ${entry.server.name}`, (revision, token) =>
    setServerMaxConnections(target(entry), null, revision, token),
  )
}

function poolState(pool: MonitoringPool): string {
  if (pool.availableEndpoints === 0) return 'unavailable'
  if (pool.availableEndpoints < pool.totalEndpoints) return 'degraded'
  return 'available'
}

function serverKey(pool: string, server: string): string {
  return `${pool}:${server}`
}

function shortRevision(revision: string | null): string {
  if (!revision) return 'None'
  return revision.length > 16 ? `${revision.slice(0, 12)}...${revision.slice(-4)}` : revision
}

function humanize(value: string): string {
  return value.replaceAll('_', ' ')
}

function errorMessage(value: unknown, fallback: string): string {
  return value instanceof Error ? value.message : fallback
}

watch(() => props.token, (token) => {
  if (!token) {
    controller?.abort()
    status.value = null
    generation.value = null
    listeners.value = null
    pools.value = null
    servers.value = null
    error.value = null
    message.value = null
    return
  }
  void load()
})

onMounted(() => {
  if (props.token) void load()
})

onBeforeUnmount(() => {
  controller?.abort()
})
</script>

<style scoped>
.operations-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.section-heading,
.panel-heading,
.server-heading,
.action-row,
.capacity-form {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
}

.workspace-heading,
.section-heading {
  align-items: flex-end;
}

.workspace-heading {
  margin-bottom: 28px;
}

.eyebrow,
.label {
  margin: 0;
  color: #929a88;
  font: 700 0.66rem/1.2 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
p {
  margin-top: 0;
}

h2,
h3 {
  font-family: Georgia, "Times New Roman", serif;
  font-weight: 400;
  letter-spacing: -0.04em;
}

h2 {
  margin: 4px 0 0;
  font-size: clamp(2.2rem, 5vw, 4rem);
}

h3 {
  margin: 4px 0 0;
  font-size: clamp(1.35rem, 3vw, 2.1rem);
}

.workspace-deck {
  max-width: 680px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.status-section,
.generation-panel,
.inventory-panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 220px;
  padding: clamp(24px, 5vw, 48px);
}

.auth-panel p,
.loading-panel p {
  margin: 7px 0 0;
  color: #8e9686;
}

.capability-index {
  color: #ffbf4b;
  font: clamp(3.5rem, 9vw, 7rem) Georgia, "Times New Roman", serif;
}

.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: operation-spin 800ms linear infinite;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
}

.error-notice {
  border-color: #ff745c;
}

.success-notice {
  border-color: #b6ff51;
}

.status-section,
.generation-panel {
  margin-top: 22px;
  padding: 22px;
}

.status-badge,
.state-chip,
.health-chip {
  padding: 5px 8px;
  border: 1px solid #536544;
  color: #c7ef94;
  font: 700 0.63rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: uppercase;
}

.status-badge.degraded,
.state-maintenance,
.health-unhealthy {
  border-color: #81483f;
  color: #ff9b88;
}

.status-badge.draining,
.state-drain,
.health-unknown {
  border-color: #806f47;
  color: #ffcf70;
}

.status-grid,
.generation-history,
.inventory-grid {
  display: grid;
  gap: 1px;
  background: #34392f;
}

.status-grid {
  grid-template-columns: repeat(4, minmax(0, 1fr));
  margin-top: 18px;
}

.status-card,
.generation-record {
  display: grid;
  gap: 8px;
  min-width: 0;
  padding: 16px;
  background: #0e110d;
}

.status-card code,
.generation-record code,
.inventory-identity code {
  overflow-wrap: anywhere;
  color: #dce3d4;
  font: 0.74rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.status-card small,
.generation-record strong,
.panel-note,
.inventory-detail,
.empty-list {
  color: #8e9686;
  font-size: 0.72rem;
}

.warning-copy {
  color: #ff9b88;
}

.generation-history {
  grid-template-columns: repeat(4, minmax(0, 1fr));
  margin-top: 18px;
}

.generation-record strong {
  font-weight: 500;
}

.action-row {
  justify-content: flex-start;
  flex-wrap: wrap;
  margin-top: 18px;
}

.primary-button,
.secondary-button,
.danger-button,
.shutdown-button,
.small-button {
  min-height: 41px;
  padding: 10px 13px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font-weight: 700;
}

.primary-button {
  border-color: #b6ff51;
  color: #11150c;
  background: #b6ff51;
}

.danger-button,
.shutdown-button {
  border-color: #75483f;
  color: #ff9b88;
}

.shutdown-button {
  background: #241410;
}

button:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

.inventory-grid {
  grid-template-columns: repeat(2, minmax(0, 1fr));
  margin-top: 22px;
}

.inventory-panel {
  min-width: 0;
  border: 0;
  background: #10130e;
}

.server-panel {
  grid-column: 1 / -1;
}

.panel-heading {
  align-items: flex-end;
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.inventory-row,
.server-row {
  min-width: 0;
  padding: 17px 20px;
  border-bottom: 1px solid #292e26;
}

.inventory-row:last-child,
.server-row:last-child {
  border-bottom: 0;
}

.inventory-identity {
  display: grid;
  gap: 6px;
  min-width: 0;
}

.inventory-identity strong {
  font-size: 0.92rem;
}

.inventory-detail {
  display: flex;
  flex-wrap: wrap;
  gap: 10px 16px;
  margin-top: 10px;
}

.compact-actions {
  margin-top: 12px;
}

.small-button {
  min-height: 36px;
  padding: 7px 9px;
  font-size: 0.7rem;
}

.server-heading {
  align-items: flex-start;
}

.health-chip {
  border-color: #607d4c;
  color: #b6ff51;
}

.health-unchecked {
  border-color: #5f6a56;
  color: #c5d1b8;
}

.server-controls {
  display: grid;
  gap: 10px;
}

.capacity-form {
  justify-content: flex-start;
  flex-wrap: wrap;
  margin-top: 4px;
}

.capacity-form label {
  color: #929a88;
  font-size: 0.7rem;
}

.capacity-form input {
  width: 150px;
  min-height: 36px;
  padding: 7px 9px;
  border: 1px solid #42493c;
  color: #eef2e7;
  background: #0d100c;
  font: 0.74rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.empty-list {
  margin: 0;
  padding: 22px 20px;
}

button:focus-visible,
input:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

@keyframes operation-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 900px) {
  .status-grid,
  .generation-history {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .section-heading,
  .server-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .status-grid,
  .generation-history,
  .inventory-grid {
    grid-template-columns: 1fr;
  }

  .server-panel {
    grid-column: auto;
  }

  .auth-panel,
  .loading-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .action-row,
  .compact-actions,
  .capacity-form {
    align-items: stretch;
    flex-direction: column;
  }

  .action-row button,
  .capacity-form input,
  .capacity-form button {
    width: 100%;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
