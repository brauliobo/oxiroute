<script setup vapor lang="ts">
import Badge from '#design/badge/Badge.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Card from '#design/panel/Card.vue'
import Panel from '#design/panel/Panel.vue'
import type { RtmpClientControlTarget, RtmpClientSnapshot, RtmpStats } from './api'
import { formatSnapshotTime } from './formatters'
import './overview/rtmp.css'

defineProps<{
  stats: RtmpStats | null
  busySessionId: string | null
}>()

const emit = defineEmits<{
  drop: [client: RtmpClientSnapshot, target: RtmpClientControlTarget]
}>()

const details = (client: RtmpClientSnapshot) =>
  [client.stream && `Stream ${client.stream}`, client.peerIp, `Revision ${client.revision}`].filter(Boolean).join(' / ')
</script>

<template lang="pug">
Panel.ox-view-section(title="Connected RTMP clients")
  p.ox-muted(v-if="stats") Snapshot: {{ formatSnapshotTime(stats.asOfUnixMs) }}
  EmptyState(v-if="!stats || stats.clients.length === 0" title="No connected RTMP clients") Client sessions appear here after an RTMP CONNECT is accepted.
  .ox-cards(v-else)
    Card(v-for="client in stats.clients" :key="client.id")
      .ox-card-head
        div
          h3 {{ client.application || 'Unbound session' }}
          p.ox-muted {{ client.service }}
        Badge(:tone="client.role === 'publisher' ? 'info' : 'neutral'" :label="client.role.toUpperCase()")
      p.ox-muted {{ details(client) }}
      code.ox-mono {{ client.id }}
      .ox-client-actions
        Button(
          variant="danger"
          small
          :disabled="busySessionId === client.id"
          @click="emit('drop', client, 'client')"
        ) {{ busySessionId === client.id ? 'Dropping...' : 'Drop client' }}
        Button(
          v-if="client.role === 'publisher' || client.role === 'subscriber'"
          small
          :disabled="busySessionId === client.id"
          @click="emit('drop', client, client.role)"
        ) Drop {{ client.role }}
  p.ox-muted(v-if="stats?.clientsTruncated") Showing the first 1,024 clients.
</template>
