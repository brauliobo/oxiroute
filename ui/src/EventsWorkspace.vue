<template lang="pug">
section.events-workspace(aria-labelledby="events-heading" :aria-busy="loading || resyncing")
  header.workspace-heading
    div
      p.eyebrow Bounded operational history
      h2#events-heading Events
      p.workspace-deck Recent control-plane events are retained in a bounded in-memory ring. The live connection reconnects from its last cursor and resynchronizes when history rolls over.
    .event-actions
      span.stream-state(:class="`stream-${streamState}`" role="status") {{ streamStateLabel }}
      button.secondary-button(type="button" :disabled="loading || !token" @click="reloadHistory") {{ loading ? 'Loading...' : 'Reload history' }}

  .auth-panel(v-if="!token" role="status")
    span.capability-index AUTH
    div
      h3 Management token required
      p Enter the in-memory bearer token above to read the bounded event history.

  .loading-panel(v-else-if="loading && events.length === 0" role="status" aria-live="polite")
    span.loading-mark(aria-hidden="true")
    div
      strong Reading event history
      p Waiting for the control plane's bounded cursor page.

  p.notice.error-notice(v-if="error" role="alert")
    strong Event history unavailable.
    |  {{ error }}
  p.notice.success-notice(v-if="message" role="status" aria-live="polite") {{ message }}

  section.history-panel(v-if="token" aria-labelledby="history-heading")
    header.panel-heading
      div
        p.eyebrow Cursor {{ cursor }}
        h3#history-heading Recent operations
      span.panel-note(v-if="oldestCursor !== null") Oldest retained {{ oldestCursor }}
    .resync-banner(v-if="resyncing || resynced" role="status") {{ resyncing ? 'History rolled over. Reloading from the retained cursor.' : 'History was resynchronized from the retained cursor.' }}
    p.empty-list(v-if="events.length === 0 && !loading") No operational events have been retained.
    .event-list(v-else)
      article.event-row(v-for="event in events" :key="event.cursor")
        .event-cursor
          strong {{ event.cursor }}
          span {{ event.timestampUnixMs === null ? 'No timestamp' : formatTime(event.timestampUnixMs) }}
        .event-content
          .event-heading
            strong {{ eventLabel(event.event) }}
            span.outcome-chip(:class="`outcome-${event.outcome}`") {{ event.outcome }}
          p.event-meta(v-if="event.revision") Revision {{ shortRevision(event.revision) }}
          p.event-meta(v-if="event.certificate") Certificate {{ event.certificate }}
    .history-actions(v-if="hasMore")
      button.secondary-button(type="button" :disabled="loading" @click="loadNextPage") Load more events
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import {
  ApiError,
  connectEventStream,
  fetchEvents,
  type EventPage,
  type EventStreamClient,
  type OperationalEvent,
  type OperationalEventName,
} from './api'
import { formatTime, presentApiError, shortRevision } from './formatters'
import { useLatestAbortableTask } from './useLatestAbortableTask'

const PAGE_SIZE = 100
const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const events = ref<OperationalEvent[]>([])
const cursor = ref(0)
const latestCursor = ref(0)
const oldestCursor = ref<number | null>(null)
const hasMore = ref(false)
const resyncing = ref(false)
const resynced = ref(false)
const error = ref<string | null>(null)
const message = ref<string | null>(null)
const streamState = ref<'offline' | 'connecting' | 'live' | 'reconnecting'>('offline')
let stream: EventStreamClient | null = null
const { loading, run: runLoad, cancel: cancelLoad } = useLatestAbortableTask()

const streamStateLabel = computed(() => ({
  offline: 'Live updates offline',
  connecting: 'Connecting to live updates',
  live: 'Live updates connected',
  reconnecting: 'Reconnecting live updates',
}[streamState.value]))

async function reloadHistory(): Promise<void> {
  if (!props.token) return
  closeStream()
  resynced.value = false
  const current = await loadPage(0, true)
  if (current && props.token) startStream()
}

async function loadNextPage(): Promise<void> {
  await loadPage(cursor.value, false)
}

async function loadPage(after: number, replace: boolean): Promise<boolean> {
  const token = props.token
  if (!token) return false
  error.value = null
  return runLoad(
    (signal) => fetchEvents(after, PAGE_SIZE, token, signal),
    (page) => applyPage(page, replace),
    (requestError) => {
      if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
      error.value = presentApiError(requestError, 'The event history route did not respond.')
    },
  )
}

function applyPage(page: EventPage, replace: boolean): void {
  const next = replace ? page.events : mergeEvents(events.value, page.events)
  events.value = next
  cursor.value = page.cursor
  latestCursor.value = page.latestCursor
  oldestCursor.value = page.oldestCursor
  hasMore.value = page.hasMore
}

function startStream(): void {
  if (!props.token || stream) return
  streamState.value = 'connecting'
  const client = connectEventStream(props.token, {
    onReady: (readyCursor) => {
      latestCursor.value = Math.max(latestCursor.value, readyCursor)
      streamState.value = 'live'
    },
    onEvent: (event) => {
      events.value = mergeEvents(events.value, [event])
      cursor.value = Math.max(cursor.value, event.cursor)
      latestCursor.value = Math.max(latestCursor.value, event.cursor)
    },
    onResyncRequired: async (data) => {
      resyncing.value = true
      resynced.value = true
      message.value = `Live history was older than the retained ring (latest cursor ${data.latestCursor}).`
      const current = await loadPage(data.oldestCursor === null ? 0 : Math.max(0, data.oldestCursor - 1), true)
      if (current) resyncing.value = false
    },
    onShutdown: () => {
      streamState.value = 'offline'
      message.value = 'The server closed live updates because it is shutting down.'
    },
    onError: (streamError) => {
      if (streamError instanceof ApiError && streamError.status === 401) emit('unauthorized')
      streamState.value = 'reconnecting'
      error.value = presentApiError(streamError, 'Live event updates are reconnecting.')
    },
  }, { after: cursor.value, maxRetries: 5 })
  stream = client
  void client.closed.then(() => {
    if (stream !== client) return
    stream = null
    if (streamState.value !== 'offline') streamState.value = 'offline'
  })
}

function closeStream(): void {
  stream?.close()
  stream = null
  streamState.value = 'offline'
}

function mergeEvents(current: OperationalEvent[], incoming: OperationalEvent[]): OperationalEvent[] {
  const byCursor = new Map(current.map((event) => [event.cursor, event]))
  for (const event of incoming) byCursor.set(event.cursor, event)
  return [...byCursor.values()].sort((left, right) => left.cursor - right.cursor).slice(-PAGE_SIZE)
}

function eventLabel(event: OperationalEventName): string {
  return event.replaceAll('_', ' ')
}

watch(() => props.token, async (token) => {
  closeStream()
  cancelLoad()
  events.value = []
  cursor.value = 0
  latestCursor.value = 0
  oldestCursor.value = null
  hasMore.value = false
  error.value = null
  message.value = null
  resyncing.value = false
  resynced.value = false
  if (!token) return
  const current = await loadPage(0, true)
  if (current) startStream()
})

onMounted(async () => {
  if (!props.token) return
  const current = await loadPage(0, true)
  if (current) startStream()
})

onBeforeUnmount(() => {
  closeStream()
})
</script>

<style scoped>
.events-workspace {
  padding: clamp(34px, 6vw, 68px) 0 58px;
}

.workspace-heading,
.panel-heading,
.event-heading,
.event-actions,
.history-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.workspace-heading {
  align-items: flex-end;
  margin-bottom: 28px;
}

.event-actions {
  flex-wrap: wrap;
  justify-content: flex-end;
}

.eyebrow,
.label {
  margin: 0;
  color: #929a88;
  font: 700 0.66rem/1.2 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  letter-spacing: 0.13em;
  text-transform: uppercase;
}

h2,
h3,
p {
  margin-top: 0;
}

h2,
h3 {
  font-family: Georgia, "Times New Roman", serif;
  font-weight: 400;
  letter-spacing: -0.04em;
}

h2 {
  margin: 4px 0 0;
  font-size: clamp(2.2rem, 5vw, 4rem);
}

h3 {
  margin: 4px 0 0;
  font-size: clamp(1.35rem, 3vw, 2.1rem);
}

.workspace-deck {
  max-width: 700px;
  margin: 12px 0 0;
  color: #8e9686;
  line-height: 1.55;
}

.auth-panel,
.loading-panel,
.history-panel {
  border: 1px solid #3a4034;
  background: rgb(16 19 14 / 86%);
}

.auth-panel,
.loading-panel {
  display: flex;
  align-items: center;
  gap: 22px;
  min-height: 220px;
  padding: clamp(24px, 5vw, 48px);
}

.auth-panel p,
.loading-panel p {
  margin: 7px 0 0;
  color: #8e9686;
}

.capability-index {
  color: #ffbf4b;
  font: clamp(3.5rem, 9vw, 7rem) Georgia, "Times New Roman", serif;
}

.loading-mark {
  width: 20px;
  height: 20px;
  border: 2px solid #596051;
  border-top-color: #b6ff51;
  border-radius: 50%;
  animation: event-spin 800ms linear infinite;
}

.stream-state,
.outcome-chip {
  padding: 5px 8px;
  border: 1px solid #536544;
  color: #c7ef94;
  font: 700 0.63rem/1 "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
  text-transform: uppercase;
}

.stream-reconnecting,
.stream-offline,
.outcome-failed,
.outcome-rejected {
  border-color: #81483f;
  color: #ff9b88;
}

.stream-connecting,
.outcome-requested,
.outcome-prepared {
  border-color: #806f47;
  color: #ffcf70;
}

.notice {
  margin: 18px 0 0;
  padding: 13px 16px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
}

.error-notice {
  border-color: #ff745c;
}

.success-notice,
.resync-banner {
  border-color: #b6ff51;
}

.panel-heading {
  align-items: flex-end;
  padding: 20px;
  border-bottom: 1px solid #34392f;
}

.panel-note,
.event-meta,
.empty-list {
  color: #8e9686;
  font-size: 0.72rem;
}

.resync-banner {
  margin: 16px 20px 0;
  padding: 11px 13px;
  border-left: 3px solid;
  background: #171a15;
  color: #cdd2c5;
}

.empty-list {
  margin: 0;
  padding: 22px 20px;
}

.event-list {
  display: grid;
}

.event-row {
  display: grid;
  grid-template-columns: 110px minmax(0, 1fr);
  gap: 18px;
  padding: 18px 20px;
  border-bottom: 1px solid #292e26;
}

.event-cursor {
  display: grid;
  align-content: start;
  gap: 5px;
}

.event-cursor strong {
  color: #b6ff51;
  font: 0.86rem "IBM Plex Mono", "SFMono-Regular", Consolas, monospace;
}

.event-cursor span,
.event-meta {
  color: #777f70;
  font-size: 0.7rem;
}

.event-heading {
  justify-content: flex-start;
}

.event-heading strong {
  color: #e1e7da;
  font-size: 0.86rem;
  text-transform: capitalize;
}

.event-meta {
  margin: 9px 0 0;
}

.history-actions {
  justify-content: flex-start;
  padding: 16px 20px;
}

.secondary-button {
  min-height: 41px;
  padding: 10px 14px;
  border: 1px solid #56604f;
  color: #c8cfc0;
  background: transparent;
  cursor: pointer;
  font-weight: 700;
}

button:disabled {
  border-color: #454b40;
  color: #777e71;
  background: #242820;
  cursor: not-allowed;
}

button:focus-visible {
  outline: 2px solid #fff;
  outline-offset: 2px;
}

@keyframes event-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 700px) {
  .workspace-heading,
  .panel-heading {
    align-items: flex-start;
    flex-direction: column;
  }

  .event-actions {
    width: 100%;
    align-items: stretch;
    flex-direction: column;
  }

  .event-actions > * {
    width: 100%;
  }

  .auth-panel,
  .loading-panel {
    align-items: flex-start;
    flex-direction: column;
  }

  .event-row {
    grid-template-columns: 1fr;
    gap: 10px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .loading-mark {
    animation: none;
  }
}
</style>
