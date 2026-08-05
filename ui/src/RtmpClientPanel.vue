<template lang="pug">
section.client-section(aria-labelledby="client-heading")
  .section-heading
    div
      p.eyebrow Session control
      h2#client-heading Connected RTMP clients
    p.snapshot-time(v-if="stats") Snapshot {{ formatTime(stats.asOfUnixMs) }}

  .client-empty(v-if="!stats || stats.clients.length === 0")
    span.empty-mark 00
    div
      h3 No connected RTMP clients
      p Client sessions appear here after an RTMP CONNECT is accepted.

  .client-grid(v-else)
    article.client-card(v-for="client in stats.clients" :key="client.id")
      header.client-header
        div
          p.client-service {{ client.service }}
          h3 {{ client.application || 'Unbound session' }}
        span.role-pill(:class="`role-${client.role}`") {{ client.role }}
      .client-details
        span(v-if="client.stream") Stream {{ client.stream }}
        span(v-if="client.peerIp") {{ client.peerIp }}
        code {{ client.id }}
        span Revision {{ client.revision }}
      .client-actions
        button.client-action.client-action-warn(
          type="button"
          :disabled="busySessionId === client.id"
          @click="$emit('drop', client, 'client')"
        ) {{ busySessionId === client.id ? 'Dropping...' : 'Drop client' }}
        button.client-action(
          v-if="client.role === 'publisher' || client.role === 'subscriber'"
          type="button"
          :disabled="busySessionId === client.id"
          @click="$emit('drop', client, client.role)"
        ) Drop {{ client.role }}

  p.client-truncation(v-if="stats?.clientsTruncated") Showing the first 1,024 clients.
</template>

<script setup lang="ts">
import type { RtmpClientControlTarget, RtmpClientSnapshot, RtmpStats } from './api'

defineProps<{
  stats: RtmpStats | null
  busySessionId: string | null
}>()

defineEmits<{
  drop: [client: RtmpClientSnapshot, target: RtmpClientControlTarget]
}>()

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(timestamp)
}
</script>

<style scoped>
.client-section {
  margin-top: 2.75rem;
  padding-top: 2rem;
  border-top: 1px solid rgba(135, 161, 166, 0.18);
}

.client-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
  gap: 0.8rem;
}

.client-card {
  padding: 1.15rem;
  border: 1px solid rgba(135, 161, 166, 0.18);
  background: rgba(13, 26, 31, 0.66);
}

.client-header,
.client-details,
.client-actions {
  display: flex;
  align-items: center;
  gap: 0.6rem;
}

.client-header {
  justify-content: space-between;
  align-items: flex-start;
}

.client-service,
.client-details,
.client-truncation {
  color: #8ea9ad;
  font-size: 0.76rem;
}

.client-service {
  margin: 0;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.client-header h3 {
  margin: 0.25rem 0 0;
  color: #e2f0ee;
  font-size: 1rem;
}

.role-pill {
  padding: 0.22rem 0.45rem;
  color: #b7d0cf;
  border: 1px solid rgba(183, 208, 207, 0.3);
  font-size: 0.68rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.role-publisher {
  color: #f0c58c;
  border-color: rgba(240, 197, 140, 0.45);
}

.role-subscriber {
  color: #90d3c0;
  border-color: rgba(144, 211, 192, 0.45);
}

.client-details {
  flex-wrap: wrap;
  margin: 1rem 0;
}

.client-details code {
  overflow: hidden;
  max-width: 100%;
  color: #a6c0c1;
  text-overflow: ellipsis;
}

.client-actions {
  flex-wrap: wrap;
}

.client-action {
  padding: 0.45rem 0.65rem;
  color: #c6ddda;
  border: 1px solid rgba(144, 211, 192, 0.35);
  background: transparent;
  cursor: pointer;
  font: inherit;
  font-size: 0.75rem;
}

.client-action:hover:not(:disabled) {
  border-color: #90d3c0;
  background: rgba(144, 211, 192, 0.08);
}

.client-action-warn {
  color: #f0c58c;
  border-color: rgba(240, 197, 140, 0.35);
}

.client-action:disabled {
  cursor: wait;
  opacity: 0.55;
}

.client-empty {
  display: flex;
  align-items: center;
  gap: 1rem;
  min-height: 7rem;
  padding: 1.25rem;
  border: 1px dashed rgba(135, 161, 166, 0.24);
}

.client-empty h3 {
  margin: 0 0 0.25rem;
  color: #dceae7;
  font-size: 1rem;
}

.client-empty p {
  margin: 0;
  color: #8ea9ad;
  font-size: 0.85rem;
}

.client-truncation {
  margin: 0.8rem 0 0;
}

@media (max-width: 640px) {
  .client-details {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
