<script setup vapor lang="ts">
import { computed } from 'vue'
import Badge from '#design/badge/Badge.vue'
import type { Tone } from '#design/badge/tone'
import Panel from '#design/panel/Panel.vue'
import type { CertbotWatcherSnapshot } from '../api'
import { formatCount } from '../formatters'
import Facts from '../workspace/Facts.vue'

const HEALTH: Readonly<Record<CertbotWatcherSnapshot['health'], { tone: Tone; label: string }>> = {
  healthy: { tone: 'ok', label: 'Healthy' },
  degraded: { tone: 'warn', label: 'Degraded' },
  stopped: { tone: 'bad', label: 'Stopped' },
}

const props = defineProps<{ watcher: CertbotWatcherSnapshot }>()
const health = computed(() => HEALTH[props.watcher.health])
const facts = computed(() => [
  { label: 'Backend errors', value: formatCount(props.watcher.backendErrors) },
  { label: 'Reconciliations failed', value: formatCount(props.watcher.reconciliationFailures) },
  { label: 'Refreshes', value: formatCount(props.watcher.watchRefreshes) },
  { label: 'Rescans', value: formatCount(props.watcher.rescans) },
])
</script>

<template lang="pug">
Panel(title="Certbot lineage watcher")
  p.ox-panel-note
    Badge(:tone="health.tone" :label="health.label")
  Facts(:items="facts")
</template>
