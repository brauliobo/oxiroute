import { reactive, ref, shallowRef } from 'vue'
import {
  ApiError,
  fetchInflightRequests,
  INFLIGHT_LIMIT_MAX,
  type InflightPhase,
  type InflightRequestFilter,
  type InflightRequestList,
} from '../api'
import { presentApiError } from '../formatters'
import { useLatestAbortableTask } from '../useLatestAbortableTask'

export interface InflightDraft {
  pool: string
  route: string
  phase: string
  minAgeSeconds: string
  limit: string
  requestId: string
}

export const DEFAULT_LIMIT = '100'

export function emptyDraft(): InflightDraft {
  return { pool: '', route: '', phase: '', minAgeSeconds: '', limit: DEFAULT_LIMIT, requestId: '' }
}

export function draftFilter(draft: InflightDraft): InflightRequestFilter | string {
  const limit = Number(draft.limit)
  if (!Number.isInteger(limit) || limit < 1 || limit > INFLIGHT_LIMIT_MAX) {
    return `The limit must be a whole number from 1 to ${INFLIGHT_LIMIT_MAX}.`
  }
  const minAgeSeconds = draft.minAgeSeconds === '' ? 0 : Number(draft.minAgeSeconds)
  if (!Number.isFinite(minAgeSeconds) || minAgeSeconds < 0) return 'The minimum age must be zero or more seconds.'
  return {
    pool: draft.pool.trim() || undefined,
    route: draft.route.trim() || undefined,
    phase: (draft.phase || undefined) as InflightPhase | undefined,
    minAgeMs: Math.round(minAgeSeconds * 1000) || undefined,
    limit,
    requestId: draft.requestId.trim() || undefined,
  }
}

export function useInflightRequests(token: () => string, onUnauthorized: () => void) {
  const draft = reactive(emptyDraft())
  const list = shallowRef<InflightRequestList | null>(null)
  const error = ref<string | null>(null)
  const { loading, run, cancel } = useLatestAbortableTask()

  async function load(): Promise<void> {
    const filter = draftFilter(draft)
    if (typeof filter === 'string') {
      error.value = filter
      return
    }
    error.value = null
    await run(
      (signal) => fetchInflightRequests(token(), filter, signal),
      (next) => { list.value = next },
      (failure) => {
        if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
        error.value = presentApiError(failure, 'The requests API did not respond.')
      },
    )
  }

  function resetFilters(): Promise<void> {
    Object.assign(draft, emptyDraft())
    return load()
  }

  function reset(): void {
    cancel()
    list.value = null
    error.value = null
    Object.assign(draft, emptyDraft())
  }

  return { draft, list, error, loading, load, resetFilters, reset }
}
