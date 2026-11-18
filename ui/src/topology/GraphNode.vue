<script setup vapor lang="ts">
import { computed } from 'vue'
import Button from '#design/button/Button.vue'
import { vCssVars } from '#design/directives/cssVars'
import StatusBadge from '#design/status/StatusBadge.vue'
import { statusOfRuntimeState, toneOf } from '#design/status/status'
import type { TopologyNode, TopologyRuntimeOverlay } from '../api'
import { formatCount } from '../formatters'
import { kindLabels } from './labels'
import { activeConnections, identityLabel } from './identity'
import Icon from './Icon.vue'
import './styles.css'

const props = defineProps<{
  node: TopologyNode
  overlay?: TopologyRuntimeOverlay
  selected: boolean
  vars: Record<string, string | number>
}>()

const emit = defineEmits<{ select: [] }>()

const identity = computed(() => identityLabel(props.node))
const status = computed(() => (props.overlay ? statusOfRuntimeState(props.overlay.state) : null))
const tone = computed(() => (status.value ? toneOf(status.value) : 'neutral'))
const connections = computed(() => {
  const count = activeConnections(props.overlay)
  return count === null ? '' : `${formatCount(count)} active connections`
})
</script>

<template lang="pug">
.ox-node-slot(v-css-vars="vars")
  Button.ox-node(:pressed="selected" :data-node-id="node.id" @click="emit('select')")
    Icon(:class="`ox-tone-${tone}`" :kind="node.kind")
    span.ox-node-copy
      span.ox-node-kind {{ kindLabels[node.kind] }}
      strong {{ node.name }}
      span(v-if="identity") {{ identity }}
      StatusBadge(v-if="status" :status="status")
      span(v-if="connections") {{ connections }}
</template>
