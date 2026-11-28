<template lang="pug">
Panel(title="Route debugger")
  p.ox-muted Test how the active HTTP and HTTP/3 listeners match a request without sending live traffic.
  .ox-ops-filters
    Field(label="Path")
      Input(v-model="path" placeholder="/api/v1/items")
    Field(label="Host (optional)")
      Input(v-model="host" placeholder="api.example.test")
    Field(label="Method")
      Select(v-model="method" :options="METHOD_OPTIONS")
    Field(label="Listener (optional)")
      Input(v-model="listener" placeholder="All HTTP listeners")
    .ox-ops-buttons
      Button(variant="primary" :disabled="loading || !path.trim()" @click="runExplain") {{ loading ? 'Testing...' : 'Explain route' }}

  Notice(kind="alert")
    Banner(v-if="error" tone="bad")
      strong Route explain failed.
      |  {{ error }}

  template(v-if="result")
    p.ox-muted
      | Query: {{ result.method }} {{ result.host ?? '*' }}{{ result.path }} · {{ result.matches.length }} match(es), {{ result.unmatched.length }} unmatched listener(s).
    EmptyState(v-if="result.matches.length === 0" title="No route matched")
      | Unmatched listeners: {{ result.unmatched.join(', ') || 'none' }}
    pre.ox-ops-detail(v-else) {{ JSON.stringify(result, null, 2) }}
</template>

<script setup lang="ts" vapor>
import { ref } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Panel from '#design/panel/Panel.vue'
import { ApiError, explainRoute, type RouteExplainResponse } from '../api'

const METHOD_OPTIONS = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'].map((m) => ({ value: m, label: m }))

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const path = ref('/')
const host = ref('')
const method = ref('GET')
const listener = ref('')
const loading = ref(false)
const error = ref<string | null>(null)
const result = ref<RouteExplainResponse | null>(null)

async function runExplain(): Promise<void> {
  if (!props.token || !path.value.trim()) return
  loading.value = true
  error.value = null
  try {
    result.value = await explainRoute(
      {
        path: path.value.trim(),
        ...(host.value.trim() ? { host: host.value.trim() } : {}),
        method: method.value,
        ...(listener.value.trim() ? { listener: listener.value.trim() } : {}),
      },
      props.token,
    )
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      emit('unauthorized')
      return
    }
    error.value = err instanceof Error ? err.message : 'Unexpected route explain error'
  } finally {
    loading.value = false
  }
}
</script>
