<script setup vapor lang="ts">
import { computed, ref, useId } from 'vue'
import { ADMIN_STATES, postAdminState } from '../admin'
import type { AdminRequest } from '../admin'
import type { PoolRow } from '../model'
import ModalDialog from './ModalDialog.vue'

// Ready, Drain and Maintenance for one server. The shell shows it only for a viewer the server allows to administer
// (`admin`) and a generation with an active revision, which every request carries. A request is posted after confirming.
const props = defineProps<{ pools: PoolRow[]; revision: string }>()
const emit = defineEmits<{ changed: [] }>()

const REFRESH_STATUSES = [404, 409]
const headingId = useId()
const poolName = ref<string>()
const serverName = ref<string>()
const pool = computed(() => props.pools.find((item) => item.name === poolName.value) ?? props.pools[0])
const server = computed(() => pool.value?.servers.find((item) => item.name === serverName.value) ?? pool.value?.servers[0])
const target = ref<Omit<AdminRequest, 'revision'>>()
const busy = ref(false)
const result = ref<{ ok: boolean; text: string }>()
const verb = computed(() => ADMIN_STATES.find((item) => item.state === target.value?.state))

const pickPool = (event: Event) => { poolName.value = (event.target as HTMLSelectElement).value }
const pickServer = (event: Event) => { serverName.value = (event.target as HTMLSelectElement).value }
const ask = (state: AdminRequest['state']) => {
  if (pool.value && server.value) target.value = { pool: pool.value.name, server: server.value.name, state }
}

async function confirm() {
  const request = target.value!
  target.value = undefined
  busy.value = true
  const outcome = await postAdminState(location.pathname, { ...request, revision: props.revision })
  busy.value = false
  const { past } = ADMIN_STATES.find((item) => item.state === request.state)!
  result.value = outcome.ok
    ? { ok: true, text: `Server ${request.server} of pool ${request.pool} is now ${past}.` }
    : { ok: false, text: outcome.message }
  if (outcome.ok || REFRESH_STATUSES.includes(outcome.status ?? 0)) emit('changed')
}
</script>

<template lang="pug">
section.ox-admin(:aria-labelledby="headingId")
  h2(:id="headingId") Administration
  .ox-admin-controls
    .ox-field
      label(for="ox-admin-pool") Pool
      select#ox-admin-pool.ox-input(@change="pickPool")
        option(v-for="item in pools" :key="item.name" :value="item.name" :selected="item.name === pool?.name") {{ item.name }}
    .ox-field
      label(for="ox-admin-server") Server
      select#ox-admin-server.ox-input(@change="pickServer")
        option(v-for="item in pool?.servers" :key="item.name" :value="item.name" :selected="item.name === server?.name") {{ item.name }}
    p.ox-muted(v-if="server") Current state: {{ server.wire.administrativeState }}
    .ox-field
      button.ox-btn(v-for="item in ADMIN_STATES" :key="item.state" type="button" :disabled="busy || !server" @click="ask(item.state)") {{ item.label }}
  div(role="status")
    p.ox-admin-result(v-if="result?.ok") {{ result.text }}
  div(role="alert")
    p.ox-admin-result.ox-bad-text(v-if="result && !result.ok") {{ result.text }}
  ModalDialog(:open="target !== undefined" title="Confirm administration" @close="target = undefined")
    p Set server {{ target?.server }} of pool {{ target?.pool }} to {{ verb?.label }}?
    .ox-dialog-actions
      button.ox-btn(type="button" autofocus data-close) Cancel
      button.ox-btn.ox-primary(type="button" @click="confirm") Confirm
</template>
