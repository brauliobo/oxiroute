<script setup lang="ts" vapor>
import { computed, onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Loading from '#design/empty/Loading.vue'
import Panel from '#design/panel/Panel.vue'
import Tabs from '#design/tabs/Tabs.vue'
import CapabilitiesTable from './operations/CapabilitiesTable.vue'
import { dispatch, type Command } from './operations/commands'
import ForceToggle from './operations/ForceToggle.vue'
import GenerationPanel from './operations/GenerationPanel.vue'
import InflightPanel from './operations/InflightPanel.vue'
import ListenerTable from './operations/ListenerTable.vue'
import PoolTable from './operations/PoolTable.vue'
import RegistrationsPanel from './operations/RegistrationsPanel.vue'
import RestartBanner from './operations/RestartBanner.vue'
import ServerList from './operations/ServerList.vue'
import type { Scope } from './operations/types'
import { useOperationsData } from './operations/useOperationsData'
import ActionFeedback from './workspace/ActionFeedback.vue'
import { useActionRunner } from './workspace/actions'
import './operations/page.css'

const TABS = [
  { id: 'runtime', label: 'Runtime' },
  { id: 'admission', label: 'Listeners and pools' },
  { id: 'servers', label: 'Servers' },
  { id: 'requests', label: 'In-flight requests' },
  { id: 'registrations', label: 'Registrations' },
]

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const tab = ref('runtime')
const force = ref(false)
const unauthorized = () => emit('unauthorized')
const { status, generation, listeners, pools, servers, error, loading, activeRevision, snapshot, loaded, load, reset } =
  useOperationsData(() => props.token, unauthorized)
const { outcome, running, canAct, perform, dismiss } = useActionRunner<Scope, string>({
  token: () => props.token,
  context: () => activeRevision.value,
  conflictHint: 'The runtime changed since this page loaded; the current state was reloaded. Review it and retry.',
  reload: load,
  onUnauthorized: unauthorized,
})

const busy = computed(() => loading.value || running.value)
const blocked = computed(() => !canAct.value)
const listenerList = computed(() => listeners.value?.listeners ?? [])
const poolList = computed(() => pools.value?.pools ?? [])
const serverList = computed(() => servers.value?.servers ?? [])
const poolNames = computed(() => poolList.value.map((pool) => pool.name))
const run = (command: Command) => dispatch(command, { snapshot: snapshot.value, force: force.value, perform })

watch(() => props.token, (token) => {
  reset()
  dismiss()
  if (token) void load()
})

onMounted(() => {
  if (props.token) void load()
})
</script>

<template lang="pug">
section.ox-ops(aria-labelledby="operations-heading" :aria-busy="busy")
  header.ox-ops-heading
    div
      h2#operations-heading Operations
      p.ox-muted Inspect the active process, generations, listeners, pools and upstream servers, and change their runtime state without editing the canonical configuration.
    Button(data-action="refresh-operations" :disabled="loading || !token" @click="load") {{ loading ? 'Refreshing...' : 'Refresh operations' }}

  Notice(v-if="!token" kind="status")
    Banner(tone="info")
      strong Management token required.
      |  Enter the bearer token to inspect or change the local runtime state.
  Loading(v-else-if="loading && !loaded" label="Loading operational state")

  Notice(kind="alert")
    Banner(v-if="error" tone="bad")
      strong Operations unavailable.
      |  {{ error }}

  template(v-if="token && loaded")
    RestartBanner(v-if="status" :restart="status.restartRequired")
    Tabs(:tabs="TABS" v-model="tab" label="Operations sections")
      .ox-ops-stack(v-if="tab === 'runtime'")
        ActionFeedback(:outcome="outcome" scope="generation" @dismiss="dismiss")
        GenerationPanel(:status="status" :generation="generation" :disabled="blocked" @command="run")
        CapabilitiesTable(v-if="status" :capabilities="status.capabilities")
      .ox-ops-stack(v-else-if="tab === 'admission'")
        ActionFeedback(:outcome="outcome" scope="admission" @dismiss="dismiss")
        Panel(title="Listeners")
          ListenerTable(:listeners="listenerList" :disabled="blocked" @command="run")
        Panel(title="Pools")
          ForceToggle(v-model:force="force")
          PoolTable(:pools="poolList" :disabled="blocked" @command="run")
      .ox-ops-stack(v-else-if="tab === 'servers'")
        ActionFeedback(:outcome="outcome" scope="servers" @dismiss="dismiss")
        Panel(title="Servers")
          ServerList(:servers="serverList" v-model:force="force" :disabled="blocked" @command="run")
      .ox-ops-stack(v-else-if="tab === 'requests'")
        InflightPanel(:token="token" :pools="poolNames" @unauthorized="unauthorized")
      .ox-ops-stack(v-else)
        RegistrationsPanel(:token="token" @unauthorized="unauthorized")
</template>
