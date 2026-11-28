<template lang="pug">
Panel(title="Persistence overlay")
  p.ox-muted
    | Sidecar overlays keep selected operational overrides across restarts without modifying your configuration file. Transient actions such as drain and evacuate stay runtime-only; static max_connections and weighted round-robin weights can be promoted permanently into the configuration file.
  .ox-ops-buttons
    Button(:disabled="loading" @click="load") {{ loading ? 'Refreshing...' : 'Refresh overlay' }}
    Button(
      variant="primary"
      :disabled="acting || !overlay || overlay.settings.length === 0"
      @click="promote"
    ) {{ acting ? 'Working...' : 'Promote limits & weights to config file' }}
    Button(
      variant="danger"
      :disabled="acting || !overlay || overlay.state === 'empty'"
      @click="clear"
    ) Clear overlay

  Notice(kind="alert")
    Banner(v-if="error" tone="bad")
      strong Overlay operation failed.
      |  {{ error }}
  Notice(kind="status")
    Banner(v-if="notice" tone="ok")
      strong {{ notice }}
  Loading(v-if="loading && !overlay" label="Loading overlay state")

  template(v-if="overlay")
    .ox-ops-badges
      Badge(:tone="STATE_TONE[overlay.state]" :label="`State: ${overlay.state}`")
      span.ox-muted(v-if="overlay.revision") Revision {{ shortRevision(overlay.revision) }}
    EmptyState(v-if="overlay.settings.length === 0" title="No active overlay overrides")
      | All runtime state is either ephemeral or matches the configuration file on disk.
    Table(v-else caption="Persisted sidecar overlay overrides" :columns="COLUMNS")
      tbody
        tr(v-for="(item, idx) in overlay.settings" :key="`${item.target}:${item.setting}:${idx}`")
          th.ox-name(scope="row" data-label="Target") {{ item.target }}
          td(data-label="Setting") {{ item.setting }}
          td(data-label="Value") {{ item.value }}
          td(data-label="Status")
            Badge(:tone="item.status === 'applied' ? 'ok' : 'warn'" :label="item.status")
</template>

<script setup lang="ts" vapor>
import { onMounted, ref, watch } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import Table from '#design/table/Table.vue'
import {
  ApiError,
  clearOverlay,
  fetchConfig,
  fetchOverlay,
  promoteOverlay,
  type OverlaySnapshot,
  type OverlayState,
} from '../api'
import { shortRevision } from '../formatters'
import { confirmAction } from '../shell/confirm'

const COLUMNS = ['Target', 'Setting', 'Value', 'Status']
const STATE_TONE: Record<OverlayState, 'ok' | 'warn' | 'bad' | 'info'> = {
  empty: 'info',
  applied: 'ok',
  diverged: 'warn',
  unreadable: 'bad',
}

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ changed: []; unauthorized: [] }>()

const overlay = ref<OverlaySnapshot | null>(null)
const loading = ref(false)
const acting = ref(false)
const error = ref<string | null>(null)
const notice = ref<string | null>(null)

function handleError(err: unknown): void {
  if (err instanceof ApiError && err.status === 401) {
    emit('unauthorized')
    return
  }
  error.value = err instanceof Error ? err.message : 'Unexpected overlay error'
}

async function load(): Promise<void> {
  if (!props.token) return
  loading.value = true
  error.value = null
  try {
    overlay.value = await fetchOverlay(props.token)
  } catch (err) {
    handleError(err)
  } finally {
    loading.value = false
  }
}

async function promote(): Promise<void> {
  if (!props.token) return
  const ok = await confirmAction({
    title: 'Promote overlay limits and weights into the configuration file?',
    message: 'Static max_connections and weighted round-robin weights will be written to disk and removed from the overlay. Non-promotable operational states remain in the overlay.',
    confirmLabel: 'Promote to config file',
  })
  if (!ok) return
  acting.value = true
  error.value = null
  notice.value = null
  try {
    const config = await fetchConfig(props.token)
    const result = await promoteOverlay(config.diskRevision, props.token)
    notice.value = result.outcome === 'nothing_to_promote'
      ? 'No promotable max_connections or weighted round-robin weight overrides were found in the overlay.'
      : `Promoted overlay overrides into the configuration file (${result.outcome}).`
    await load()
    emit('changed')
  } catch (err) {
    handleError(err)
  } finally {
    acting.value = false
  }
}

async function clear(): Promise<void> {
  if (!props.token) return
  const ok = await confirmAction({
    title: 'Clear the persistence overlay?',
    message: 'Removes the sidecar .overlay.json file. Live runtime state continues until the next restart.',
    confirmLabel: 'Clear overlay',
    danger: true,
  })
  if (!ok) return
  acting.value = true
  error.value = null
  notice.value = null
  try {
    await clearOverlay(props.token)
    notice.value = 'Cleared persistence overlay.'
    await load()
    emit('changed')
  } catch (err) {
    handleError(err)
  } finally {
    acting.value = false
  }
}

watch(() => props.token, (token) => {
  if (token) void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>
