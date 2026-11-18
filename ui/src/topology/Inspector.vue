<script setup vapor lang="ts">
import { computed } from 'vue'
import Button from '#design/button/Button.vue'
import Card from '#design/panel/Card.vue'
import StatusBadge from '#design/status/StatusBadge.vue'
import { statusOfRuntimeState } from '#design/status/status'
import type { TopologyNode, TopologyRuntimeOverlay } from '../api'
import MetricList from '../overview/MetricList.vue'
import type { Metric } from '../overview/metric'
import { describeAttributes } from './attributes'
import { kindLabels } from './labels'
import { configuredLimit, identityLabel } from './identity'
import './styles.css'

const props = defineProps<{ node: TopologyNode; overlay?: TopologyRuntimeOverlay }>()
const emit = defineEmits<{ close: [] }>()

const identity = computed<Metric[]>(() => {
  const { node } = props
  const bind = identityLabel(node)
  const limit = configuredLimit(node)
  return [
    { label: 'Stable ID', value: node.id, mono: true },
    { label: 'Config path', value: node.configPath, mono: true },
    ...(bind ? [{ label: node.kind === 'endpoint' ? 'Endpoint identity' : 'Bind identity', value: bind }] : []),
    ...(limit ? [limit] : []),
  ]
})
const configuration = computed(() => describeAttributes(props.node.attributes))
const runtime = computed(() => describeAttributes(props.overlay?.metrics ?? {}))
</script>

<template lang="pug">
Card.ox-inspector(aria-label="Topology node inspector" aria-live="polite")
  .ox-card-head
    div
      p.ox-muted {{ kindLabels[node.kind] }}
      h3 {{ node.name }}
    Button(small data-inspector-close @click="emit('close')") Close
  StatusBadge(v-if="overlay" :status="statusOfRuntimeState(overlay.state)")
  MetricList(:items="identity")
  section(v-if="configuration.length > 0")
    h4 Configuration
    MetricList(:items="configuration")
  section(v-if="overlay && runtime.length > 0")
    h4 Runtime
    MetricList(:items="runtime")
</template>
