<template lang="pug">
WorkspaceSection(
  title="Events"
  description="Recent control-plane events are kept in a bounded in-memory ring. Live updates reconnect from the last cursor and resynchronize when history rolls over."
  :busy="loading || resyncing"
)
  template(v-slot:actions)
    StreamStatus(:state="streamState")
    Button(:disabled="loading" @click="reloadHistory") {{ loading ? 'Loading' : 'Reload history' }}

  Loading(v-if="loading && events.length === 0" label="Reading event history")

  Notice(v-if="error" kind="alert")
    Banner(tone="bad")
      strong Event history unavailable.
      |  {{ error }}
  Notice(v-if="ringRestart" kind="status")
    Banner(tone="warn")
      strong The event ring restarted.
      |  Earlier history was lost. The latest cursor is now {{ ringRestart.latestCursor }}, it was {{ ringRestart.previousLatestCursor }}.
  Notice(v-if="resyncing || resynced" kind="status")
    Banner(tone="info") {{ resyncing ? 'History rolled over. Reloading from the retained cursor.' : 'History was resynchronized from the retained cursor.' }}
  Notice(v-if="message" kind="status")
    Banner(tone="info") {{ message }}

  Panel(title="Recent operations")
    p.ox-panel-note Cursor {{ cursor }}, latest {{ latestCursor }}{{ oldestCursor === null ? '' : `, oldest retained ${oldestCursor}` }}
    .ox-toolbar
      Field.ox-filter(label="Search events" label-for="events-search")
        Input#events-search(v-model="search" type="search")
      Field.ox-filter(label="Event" label-for="events-name")
        Select#events-name(v-model="eventName" :options="nameOptions")
    p.ox-panel-note(v-if="events.length" role="status") {{ filtering ? `Showing ${visible.length} of ${plural(events.length, 'event')}` : plural(events.length, 'event') }}
    EmptyState(v-if="events.length === 0 && !loading" title="No events") No operational events have been retained.
    EmptyState(v-else-if="visible.length === 0 && !loading" title="No matching events") Change the search text or the event filter.
    EventsTable(v-else-if="visible.length" :events="visible")
    .ox-more(v-if="hasMore")
      Button(:disabled="loading" @click="loadNextPage") Load more events
</template>

<script setup lang="ts" vapor>
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import Select from '#design/field/Select.vue'
import Panel from '#design/panel/Panel.vue'

import { ApiError, connectEventStream, fetchEvents } from './api'
import type { EventPage, EventStreamClient, OperationalEvent } from './api'
import EventsTable from './events/EventsTable.vue'
import { ALL_EVENTS, eventNameOptions, filterEvents, ringRestarted } from './events/filter'
import StreamStatus from './events/StreamStatus.vue'
import type { RingRestart, StreamState } from './events/types'
import { presentApiError } from './formatters'
import { useLatestAbortableTask } from './useLatestAbortableTask'
import { plural } from './workspace/plural'
import WorkspaceSection from './workspace/WorkspaceSection.vue'

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
const ringRestart = ref<RingRestart | null>(null)
const error = ref<string | null>(null)
const message = ref<string | null>(null)
const streamState = ref<StreamState>('offline')
const search = ref('')
const eventName = ref(ALL_EVENTS)
let stream: EventStreamClient | null = null
const { loading, run: runLoad, cancel: cancelLoad } = useLatestAbortableTask()

const visible = computed(() => filterEvents(events.value, search.value, eventName.value))
const nameOptions = computed(() => eventNameOptions(events.value))
const filtering = computed(() => search.value.trim() !== '' || eventName.value !== ALL_EVENTS)

async function reloadHistory(): Promise<void> {
  closeStream()
  resynced.value = false
  ringRestart.value = null
  const current = await loadPage(0, true)
  if (current) startStream()
}

const loadNextPage = (): Promise<boolean> => loadPage(cursor.value, false)

function loadPage(after: number, replace: boolean): Promise<boolean> {
  error.value = null
  return runLoad(
    (signal) => fetchEvents(after, PAGE_SIZE, props.token, signal),
    (page) => applyPage(page, replace),
    (requestError) => {
      if (requestError instanceof ApiError && requestError.status === 401) emit('unauthorized')
      error.value = presentApiError(requestError, 'The event history route did not respond.')
    },
  )
}

function noteRingRestart(reportedLatestCursor: number): boolean {
  const restarted = ringRestarted(latestCursor.value, reportedLatestCursor)
  if (restarted) ringRestart.value = { previousLatestCursor: latestCursor.value, latestCursor: reportedLatestCursor }
  return restarted
}

function applyPage(page: EventPage, replace: boolean): void {
  const restarted = noteRingRestart(page.latestCursor)
  events.value = replace || restarted ? page.events : mergeEvents(events.value, page.events)
  cursor.value = page.cursor
  latestCursor.value = page.latestCursor
  oldestCursor.value = page.oldestCursor
  hasMore.value = page.hasMore
}

function onReady(readyCursor: number): void {
  streamState.value = 'live'
  if (ringRestarted(latestCursor.value, readyCursor)) void loadPage(0, true)
  else latestCursor.value = Math.max(latestCursor.value, readyCursor)
}

async function onResyncRequired(oldest: number | null): Promise<void> {
  resyncing.value = true
  resynced.value = true
  const current = await loadPage(oldest === null ? 0 : Math.max(0, oldest - 1), true)
  if (current) resyncing.value = false
}

function startStream(): void {
  if (stream) return
  streamState.value = 'connecting'
  const client = connectEventStream(props.token, {
    onReady,
    onEvent: (event) => {
      events.value = mergeEvents(events.value, [event])
      cursor.value = Math.max(cursor.value, event.cursor)
      latestCursor.value = Math.max(latestCursor.value, event.cursor)
    },
    onResyncRequired: (data) => {
      message.value = `Live history was older than the retained ring (latest cursor ${data.latestCursor}).`
      return onResyncRequired(data.oldestCursor)
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
    streamState.value = 'offline'
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

async function start(): Promise<void> {
  if (await loadPage(0, true)) startStream()
}

watch(() => props.token, () => {
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
  ringRestart.value = null
  void start()
})

onMounted(start)
onBeforeUnmount(closeStream)
</script>
