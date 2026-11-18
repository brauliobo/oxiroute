<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import Banner from '#design/banner/Banner.vue'
import Panel from '#design/panel/Panel.vue'
import type { AuditStatus } from '../api'
import { formatBytes, formatCount } from '../formatters'
import Facts from '../workspace/Facts.vue'
import { degradedCheck, storeBadge } from './model'

const props = defineProps<{ status: AuditStatus }>()

const badge = computed(() => storeBadge(props.status.state))
const degraded = computed(() => props.status.degraded || props.status.state === 'degraded')
const memoryOnly = computed(() => props.status.state === 'memory' || !props.status.persistent)
const facts = computed(() => [
  { label: 'Persistence', value: props.status.persistent ? 'Persistent' : 'Memory only' },
  { label: 'Records retained', value: `${formatCount(props.status.recordCount)} of ${formatCount(props.status.maxRecords)}` },
  { label: 'Store size', value: `${formatBytes(props.status.bytes)} of ${formatBytes(props.status.maxTotalBytes)}` },
  { label: 'Corrupt records', value: formatCount(props.status.corruptRecords) },
  { label: 'Write failures', value: formatCount(props.status.writeFailures) },
  { label: 'Rotated files', value: `${formatCount(props.status.rotatedFiles)} of ${formatCount(props.status.maxRotatedFiles)}` },
])
</script>

<template lang="pug">
Panel(title="Store health")
  .ox-stack
    p
      Badge(:tone="badge.tone" :label="badge.label")
    Banner(v-if="degraded" tone="warn")
      strong Durable audit is degraded.
      |  Persistence reported #[code.ox-mono {{ status.lastError ?? 'a bounded storage problem' }}]. {{ degradedCheck(status.lastError) }} Records shown below are only those returned by the durable route.
    Banner(v-else-if="memoryOnly" tone="warn")
      strong Durable persistence is not active.
      |  The API reports memory-only audit state. No event stream is treated as durable history.
    Facts(:items="facts")
</template>
