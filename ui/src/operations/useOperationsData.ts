import { computed, ref, shallowRef } from 'vue'
import {
  ApiError,
  fetchGenerations,
  fetchListeners,
  fetchPools,
  fetchServers,
  fetchStatus,
  type GenerationStatus,
  type ListenerInventoryResponse,
  type PoolInventoryResponse,
  type RuntimeStatus,
  type ServerInventoryResponse,
} from '../api'
import { presentApiError } from '../formatters'
import { useLatestAbortableTask } from '../useLatestAbortableTask'
import type { Snapshot } from './types'

export function useOperationsData(token: () => string, onUnauthorized: () => void) {
  const status = shallowRef<RuntimeStatus | null>(null)
  const generation = shallowRef<GenerationStatus | null>(null)
  const listeners = shallowRef<ListenerInventoryResponse | null>(null)
  const pools = shallowRef<PoolInventoryResponse | null>(null)
  const servers = shallowRef<ServerInventoryResponse | null>(null)
  const error = ref<string | null>(null)
  const { loading, run, cancel } = useLatestAbortableTask()

  const activeRevision = computed(() => generation.value?.activeRevision ?? status.value?.activeRevision ?? null)
  const snapshot = computed<Snapshot>(() => ({
    status: status.value,
    generation: generation.value,
    listeners: listeners.value,
    pools: pools.value,
    servers: servers.value,
  }))
  const loaded = computed(() => Boolean(status.value || generation.value || listeners.value || pools.value || servers.value))

  async function load(): Promise<void> {
    const bearer = token()
    if (!bearer) return
    error.value = null
    await run(
      (signal) => Promise.all([
        fetchStatus(bearer, signal),
        fetchGenerations(bearer, signal),
        fetchListeners(bearer, signal),
        fetchPools(bearer, signal),
        fetchServers(bearer, signal),
      ]),
      ([nextStatus, nextGeneration, nextListeners, nextPools, nextServers]) => {
        status.value = nextStatus
        generation.value = nextGeneration.generation
        listeners.value = nextListeners
        pools.value = nextPools
        servers.value = nextServers
      },
      (failure) => {
        if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
        error.value = presentApiError(failure, 'The operations API did not respond.')
      },
    )
  }

  function reset(): void {
    cancel()
    status.value = generation.value = listeners.value = pools.value = servers.value = null
    error.value = null
  }

  return { status, generation, listeners, pools, servers, error, loading, activeRevision, snapshot, loaded, load, reset }
}
