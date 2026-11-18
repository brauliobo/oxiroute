<script setup vapor lang="ts">
import { computed, onMounted, watch } from 'vue'
import Banner from '#design/banner/Banner.vue'
import Notice from '#design/banner/Notice.vue'
import Button from '#design/button/Button.vue'
import EmptyState from '#design/empty/EmptyState.vue'
import Loading from '#design/empty/Loading.vue'
import ActionFeedback from '../../workspace/ActionFeedback.vue'
import ScrollRegion from '../../workspace/ScrollRegion.vue'
import { listSummary, sortNote } from './filters'
import QuotaCards from './QuotaCards.vue'
import RecordingFilters from './RecordingFilters.vue'
import RecordingsTable from './RecordingsTable.vue'
import { RECORDINGS_SCOPE } from './plans'
import { useRecordingActions } from './useRecordingActions'
import { useRecordings } from './useRecordings'
import './page.css'

const props = defineProps<{ token: string }>()
const emit = defineEmits<{ unauthorized: [] }>()

const unauthorized = () => emit('unauthorized')
const data = useRecordings(() => props.token, unauthorized)
const { filters, sort, streams, codecs, roots, visible, recordings, loaded, loading, error, filtered } = data
const actions = useRecordingActions({ token: () => props.token, reload: data.load, onUnauthorized: unauthorized })

const more = computed(() => data.nextCursor.value !== null)
const busy = computed(() => loading.value || actions.running.value)
const summary = computed(() => listSummary(visible.value.length, recordings.value.length, more.value))

watch(() => props.token, (token) => {
  data.reset()
  actions.dismiss()
  if (token) void data.load()
})

onMounted(() => {
  if (props.token) void data.load()
})
</script>

<template lang="pug">
section.ox-recs(aria-label="Recordings" :aria-busy="busy")
  header.ox-recs-heading
    p.ox-muted Recordings are FLV files. Browsers cannot play FLV, so each recording is downloaded and then opened in a media player.
    Button(data-action="refresh-recordings" :disabled="loading || !token" @click="data.load") {{ loading ? 'Refreshing...' : 'Refresh' }}

  ActionFeedback(:outcome="actions.outcome.value" :scope="RECORDINGS_SCOPE" @dismiss="actions.dismiss")

  Notice(kind="alert")
    Banner(v-if="error" tone="bad" data-recordings-error)
      strong Recordings unavailable.
      |  {{ error }}
      template(v-slot:actions)
        Button(small data-action="retry-recordings" :disabled="loading" @click="data.load") Retry

  Loading(v-if="loading && !loaded" label="Loading recordings")

  template(v-if="loaded")
    QuotaCards(:roots="roots")
    RecordingFilters(
      v-if="recordings.length > 0 || filtered"
      :filters="filters"
      :sort="sort"
      :streams="streams"
      :codecs="codecs"
      :active="filtered"
      @change="data.setFilter"
      @sort="data.setSort"
      @clear="data.clearFilters"
    )
    EmptyState(v-if="recordings.length === 0 && !filtered" title="No recordings yet")
      | Recordings appear after a recorder with a root directory has written a file.
    template(v-else)
      p.ox-muted(role="status" data-list-summary) {{ summary }} {{ sortNote(recordings.length) }}
      EmptyState(v-if="visible.length === 0" title="No recording matches the filters")
        | Change or clear the filters{{ more ? ', or load more recordings to search further' : '' }}.
        Button(data-action="clear-filters-empty" @click="data.clearFilters") Clear filters
      ScrollRegion(v-else label="Recordings table")
        RecordingsTable(
          :recordings="visible"
          :sort="sort"
          :busy="busy"
          @sort="data.sortBy"
          @download="actions.download"
          @remove="actions.remove"
        )
      .ox-more(v-if="more")
        Button(data-action="load-more" :disabled="loading" @click="data.loadMore") Load more
</template>
