<script setup vapor lang="ts">
import { computed } from 'vue'
import Button from '#design/button/Button.vue'
import type { MonitoringSnapshot } from '../api'
import { formatBytes, formatCount, formatTelemetryDuration } from '../formatters'
import { formatOptionalPercent, memoryUsagePercent, totalTrafficBytes } from '../overview/telemetry'
import './styles.css'

const props = defineProps<{ monitoring: MonitoringSnapshot | null; refreshing: boolean }>()
const emit = defineEmits<{ refresh: [] }>()

const NONE = '--'
const connections = computed(() => (props.monitoring ? formatCount(props.monitoring.traffic.activeConnections) : NONE))
const traffic = computed(() => (props.monitoring ? formatBytes(totalTrafficBytes(props.monitoring.traffic)) : NONE))
const memory = computed(() => (props.monitoring ? formatOptionalPercent(memoryUsagePercent(props.monitoring.host)) : NONE))
const uptime = computed(() => (props.monitoring ? formatTelemetryDuration(props.monitoring.uptimeMs) : NONE))
</script>

<template lang="pug">
section.ox-readouts(aria-label="Live monitoring summary")
  .ox-readout
    span Active connections
    strong {{ connections }}
  .ox-readout
    span Traffic moved
    strong {{ traffic }}
  .ox-readout
    span Host memory used
    strong {{ memory }}
  .ox-readout
    span Uptime
    strong {{ uptime }}
  Button(variant="primary" data-action="refresh" :disabled="refreshing" @click="emit('refresh')") {{ refreshing ? 'Refreshing...' : 'Refresh now' }}
</template>
