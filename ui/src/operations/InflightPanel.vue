<script setup vapor lang="ts">
import { computed, onMounted, useId } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import { formatDuration } from '#design/format'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import { INFLIGHT_LIMIT_MAX, INFLIGHT_PHASES } from '../api'
import { humanize } from './labels'
import { useInflightRequests } from './useInflightRequests'
import './page.css'

const COLUMNS = ['Age', 'Request', 'Phase', 'Pool and server', 'Route', 'Client']

const props = defineProps<{ token: string; pools: readonly string[] }>()
const emit = defineEmits<{ unauthorized: [] }>()

const { draft, list, error, loading, load, resetFilters } = useInflightRequests(() => props.token, () => emit('unauthorized'))
const ids = { pool: useId(), route: useId(), phase: useId(), age: useId(), id: useId(), limit: useId() }

const poolOptions = computed(() => [{ value: '', label: 'Any pool' }, ...props.pools.map((pool) => ({ value: pool, label: pool }))])
const phaseOptions = [{ value: '', label: 'Any phase' }, ...INFLIGHT_PHASES.map((phase) => ({ value: phase, label: humanize(phase) }))]
const summary = computed(() => {
  const found = list.value
  if (!found) return ''
  const shown = `Showing ${found.requests.length} of ${found.total} requests in flight, oldest first.`
  return found.truncated ? `${shown} Narrow the filters or raise the limit to see the others.` : shown
})

onMounted(load)
</script>

<template lang="pug">
Panel.ox-ops-requests(title="In-flight requests")
  form.ox-ops-filters(@submit.prevent="load")
    Field(stacked label="Pool" :label-for="ids.pool")
      Select(:id="ids.pool" v-model="draft.pool" :options="poolOptions")
    Field(stacked label="Route" :label-for="ids.route")
      Input(:id="ids.route" v-model="draft.route" autocomplete="off")
    Field(stacked label="Phase" :label-for="ids.phase")
      Select(:id="ids.phase" v-model="draft.phase" :options="phaseOptions")
    Field(stacked label="Older than (seconds)" :label-for="ids.age")
      Input(:id="ids.age" v-model="draft.minAgeSeconds" type="number" min="0" step="1")
    Field(stacked label="Request ID" :label-for="ids.id")
      Input(:id="ids.id" v-model="draft.requestId" autocomplete="off" spellcheck="false")
    Field(stacked label="Limit" :label-for="ids.limit")
      Input(:id="ids.limit" v-model="draft.limit" type="number" min="1" :max="INFLIGHT_LIMIT_MAX" step="1")
    .ox-ops-buttons
      Button(variant="primary" type="submit" :disabled="loading") Apply filters
      Button(:disabled="loading" @click="resetFilters") Reset filters
  Notice(kind="alert")
    Banner(v-if="error" tone="bad")
      strong Requests unavailable.
      |  {{ error }}
  Loading(v-if="loading && !list" label="Reading in-flight requests")
  template(v-if="list")
    p.ox-muted(role="status") {{ summary }}
    EmptyState(v-if="list.requests.length === 0" title="No requests in flight") No in-flight request matches the filters.
    Table(v-else caption="In-flight requests, oldest first" :columns="COLUMNS")
      tbody
        tr(v-for="request in list.requests" :key="request.requestId" :data-request="request.requestId")
          td(data-label="Age")
            | {{ formatDuration(Number(request.ageMs)) }}
            Badge(v-if="request.overdue" tone="warn" label="Overdue")
          th.ox-name(scope="row" data-label="Request")
            | {{ request.method }} {{ request.path }}
            code.ox-ops-sub {{ request.requestId }}
          td(data-label="Phase") {{ humanize(request.phase) }}{{ request.queuePosition != null ? `, queue position ${request.queuePosition}` : '' }}
          td(data-label="Pool and server") {{ request.pool ?? 'not routed yet' }}{{ request.server ? ` / ${request.server}` : '' }}
          td(data-label="Route") {{ request.route ?? 'none' }}
          td(data-label="Client") {{ request.clientIp ?? 'unknown' }} via {{ request.listener }}
</template>
