<script setup lang="ts" vapor>
import { computed, ref } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import Chip from '#design/button/Chip.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import Field from '#design/field/Field.vue'
import Input from '#design/field/Input.vue'
import { playsHls } from '#design/video/support'
import type { RtmpCatalog, RtmpStats } from '../../api'
import ActionFeedback from '../../workspace/ActionFeedback.vue'
import { useActionRunner } from '../../workspace/actions'
import CameraTile from './CameraTile.vue'
import { NO_FILTER, filterCameras, summarize } from './filter'
import type { StatusFilter } from './filter'
import { buildCameras } from './model'
import { dropPublisher, dropViewer, startRecording, stopRecording } from './plans'
import type { StreamsScope } from './plans'
import { MAX_PREVIEWS, usePreviewSet } from './usePreviewSet'
import './page.css'

const CHIPS: readonly { id: StatusFilter; label: string }[] = [
  { id: 'all', label: 'All' },
  { id: 'streaming', label: 'Live' },
  { id: 'degraded', label: 'Degraded' },
  { id: 'idle', label: 'Idle' },
  { id: 'offline', label: 'Offline' },
]

const props = defineProps<{
  token: string
  catalog: RtmpCatalog | null
  stats: RtmpStats | null
  catalogError: string | null
  statsError: string | null
  nowUnixMs: number
  refreshing: boolean
  reload: () => Promise<void>
}>()
const emit = defineEmits<{ unauthorized: [] }>()

const query = ref(NO_FILTER.query)
const status = ref<StatusFilter>(NO_FILTER.status)
const previews = usePreviewSet()
const canPreview = playsHls()
const { outcome, running, perform, dismiss } = useActionRunner<StreamsScope, true>({
  token: () => props.token,
  context: () => true,
  conflictHint: 'The stream changed while the command was in flight; the current state was reloaded. Review it and retry.',
  reload: () => props.reload(),
  onUnauthorized: () => emit('unauthorized'),
})

const cameras = computed(() => (props.catalog ? buildCameras(props.catalog, props.stats, props.nowUnixMs) : []))
const summary = computed(() => summarize(cameras.value))
const shown = computed(() => filterCameras(cameras.value, { query: query.value, status: status.value }))
const filtered = computed(() => query.value !== '' || status.value !== 'all')
const counts = (id: StatusFilter) => (id === 'all' ? summary.value.all : summary.value[id])

function clearFilters(): void {
  query.value = NO_FILTER.query
  status.value = NO_FILTER.status
}
</script>

<template lang="pug">
section.ox-cams(aria-label="Streams" :aria-busy="refreshing && !catalog")
  Notice(kind="alert")
    Banner(v-if="catalogError" tone="bad")
      strong {{ catalog ? 'Stream refresh failed.' : 'Streams unavailable.' }}
      |  {{ catalogError }}{{ catalog ? ' The last valid streams stay visible.' : '' }}
    Banner(v-if="statsError" tone="bad")
      strong Client list unavailable.
      |  {{ statsError }} Publisher and viewer controls need it.
  Notice(kind="status")
    Banner(v-if="catalog && !catalog.capabilities.live_ingest" tone="warn")
      strong RTMP ingestion is not connected.
      |  Configure an RTMP listener to accept publishers.

  Loading(v-if="!catalog && refreshing" label="Loading streams")

  template(v-if="catalog")
    .ox-cams-summary
      Chip(
        v-for="chip in CHIPS"
        :key="chip.id"
        :pressed="status === chip.id"
        :data-filter="chip.id"
        @click="status = chip.id"
      ) {{ chip.label }} {{ counts(chip.id) }}
      p.ox-muted {{ summary.recording }} recording now. At most {{ MAX_PREVIEWS }} previews play at once.
    Field(label="Find a stream" label-for="cams-find")
      Input#cams-find(v-model="query" type="search" autocomplete="off")

    ActionFeedback(:outcome="outcome" scope="streams" @dismiss="dismiss")

    EmptyState(v-if="cameras.length === 0" title="No streams yet") Streams appear here while a publisher is connected and for an hour after it leaves.
    EmptyState(v-else-if="shown.length === 0" title="No stream matches")
      | Change the search or the state filter.
      Button(small @click="clearFilters") Clear filters
    ul.ox-cams-grid(v-else aria-label="Streams")
      li(v-for="camera in shown" :key="camera.id")
        CameraTile(
          :camera="camera"
          :now-unix-ms="nowUnixMs"
          :token="token"
          :previewing="previews.has(camera.id)"
          :can-preview="canPreview"
          :disabled="running"
          @preview="previews.toggle(camera.id)"
          @unauthorized="emit('unauthorized')"
          @start-recording="(recorder) => perform(startRecording(camera, recorder))"
          @stop-recording="(recorder) => perform(stopRecording(camera, recorder))"
          @drop-publisher="(client) => perform(dropPublisher(camera, client))"
          @drop-viewer="(client) => perform(dropViewer(camera, client))"
        )
</template>
