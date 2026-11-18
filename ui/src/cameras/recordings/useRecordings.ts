import { computed, reactive, ref, shallowRef } from 'vue'
import { ApiError, fetchRecordings, type Recording, type RecordingPage, type RecordingRoot } from '../../api'
import { presentApiError } from '../../formatters'
import { useLatestAbortableTask } from '../../useLatestAbortableTask'
import {
  emptyFilters, hasFilter, isServerFilter, matchesFilters, mergeSeen, serverQuery, type FilterName,
} from './filters'
import { DEFAULT_SORT, nextSort, sortRecordings, type SortKey, type SortState } from './sort'

export function useRecordings(token: () => string, onUnauthorized: () => void) {
  const filters = reactive(emptyFilters())
  const sort = ref<SortState>(DEFAULT_SORT)
  const recordings = shallowRef<readonly Recording[]>([])
  const roots = shallowRef<readonly RecordingRoot[]>([])
  const nextCursor = ref<string | null>(null)
  const streams = shallowRef<readonly string[]>([])
  const codecs = shallowRef<readonly string[]>([])
  const error = ref<string | null>(null)
  const loaded = ref(false)
  const { loading, run, cancel } = useLatestAbortableTask()

  const visible = computed(() => sortRecordings(recordings.value.filter((item) => matchesFilters(item, filters)), sort.value))
  const filtered = computed(() => hasFilter(filters))

  function receive(page: RecordingPage, append: boolean): void {
    recordings.value = append ? [...recordings.value, ...page.recordings] : page.recordings
    roots.value = page.roots
    nextCursor.value = page.next_cursor
    streams.value = mergeSeen(streams.value, page.recordings.map((item) => item.stream))
    codecs.value = mergeSeen(codecs.value, page.recordings.map((item) => item.video_codec))
    loaded.value = true
  }

  function fail(failure: unknown): void {
    if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
    error.value = presentApiError(failure, 'The recordings API did not respond.')
  }

  async function load(): Promise<void> {
    error.value = null
    await run((signal) => fetchRecordings(token(), serverQuery(filters), signal), (page) => receive(page, false), fail)
  }

  async function loadMore(): Promise<void> {
    const cursor = nextCursor.value
    if (cursor === null) return
    error.value = null
    await run((signal) => fetchRecordings(token(), serverQuery(filters, cursor), signal), (page) => receive(page, true), fail)
  }

  async function setFilter(name: FilterName, value: string): Promise<void> {
    filters[name] = value
    if (isServerFilter(name)) await load()
  }

  async function clearFilters(): Promise<void> {
    const reloads = filters.stream !== '' || filters.status !== ''
    Object.assign(filters, emptyFilters())
    if (reloads) await load()
  }

  const sortBy = (key: SortKey): void => {
    sort.value = nextSort(sort.value, key)
  }

  const setSort = (state: SortState): void => {
    sort.value = state
  }

  function reset(): void {
    cancel()
    Object.assign(filters, emptyFilters())
    sort.value = DEFAULT_SORT
    recordings.value = []
    roots.value = []
    nextCursor.value = null
    streams.value = []
    codecs.value = []
    error.value = null
    loaded.value = false
  }

  return {
    filters, sort, recordings, roots, nextCursor, streams, codecs, error, loaded, loading, visible, filtered,
    load, loadMore, setFilter, clearFilters, sortBy, setSort, reset,
  }
}
