<script setup vapor lang="ts">
import { computed, ref, useId } from 'vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Actions from '#design/dialog/Actions.vue'
import Dialog from '#design/dialog/Dialog.vue'
import Field from '#design/field/Field.vue'
import Select from '#design/field/Select.vue'
import Card from '#design/panel/Card.vue'
import { ADMIN_STATES, postAdminState } from '../admin'
import type { AdminRequest } from '../admin'
import type { PoolRow } from '../model'

const props = defineProps<{ pools: PoolRow[]; revision: string }>()
const emit = defineEmits<{ changed: [] }>()

const REFRESH_STATUSES = [404, 409]
const headingId = useId()
const poolName = ref<string>()
const serverName = ref<string>()
const pool = computed(() => props.pools.find((item) => item.name === poolName.value) ?? props.pools[0])
const server = computed(() => pool.value?.servers.find((item) => item.name === serverName.value) ?? pool.value?.servers[0])
const poolOptions = computed(() => props.pools.map((item) => ({ value: item.name, label: item.name })))
const serverOptions = computed(() => (pool.value?.servers ?? []).map((item) => ({ value: item.name, label: item.name })))
const target = ref<Omit<AdminRequest, 'revision'>>()
const busy = ref(false)
const result = ref<{ ok: boolean; text: string }>()
const verb = computed(() => ADMIN_STATES.find((item) => item.state === target.value?.state))

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
Card.ox-admin(:aria-labelledby="headingId")
  h2(:id="headingId") Administration
  p.ox-muted Runtime only — changes here are never written to configuration files.
  .ox-admin-controls
    Field(label="Pool" label-for="ox-admin-pool")
      Select#ox-admin-pool(:model-value="pool?.name ?? ''" :options="poolOptions" @update:modelValue="(name) => (poolName = name)")
    Field(label="Server" label-for="ox-admin-server")
      Select#ox-admin-server(:model-value="server?.name ?? ''" :options="serverOptions" @update:modelValue="(name) => (serverName = name)")
    p.ox-muted(v-if="server") Current state: {{ server.wire.administrativeState }}
    Field
      Button(v-for="item in ADMIN_STATES" :key="item.state" :disabled="busy || !server" @click="ask(item.state)") {{ item.label }}
  Notice(kind="status")
    p.ox-admin-result(v-if="result?.ok") {{ result.text }}
  Notice(kind="alert")
    p.ox-admin-result.ox-bad-text(v-if="result && !result.ok") {{ result.text }}
  Dialog(:open="target !== undefined" title="Confirm administration" @close="target = undefined")
    p Set server {{ target?.server }} of pool {{ target?.pool }} to {{ verb?.label }}?
    p.ox-muted Runtime only — never modifies configuration files.
    Actions
      Button(autofocus data-close) Cancel
      Button(variant="primary" @click="confirm") Confirm
</template>
