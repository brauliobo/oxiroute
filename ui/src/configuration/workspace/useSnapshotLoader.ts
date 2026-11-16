import { ref, type Ref } from 'vue'

import { ApiError, connectEventStream, fetchConfig, type EventStreamClient } from '../../api'
import type { CanonicalConfig, ConfigDiagnostic, ConfigSnapshot } from '../../config'
import {
  canonicalUnavailablePayload,
  configRouteUnavailable,
  type CanonicalUnavailableState,
} from './loadErrors'

interface SnapshotLoaderOptions {
  accessToken: Ref<string | null>
  snapshot: Ref<ConfigSnapshot | null>
  draft: Ref<CanonicalConfig | null>
  diskDiagnostics: Ref<ConfigDiagnostic[]>
  staleRevision: Ref<string | null>
  syncSnapshot: (next: ConfigSnapshot, force: boolean) => boolean
  ensureSelection: () => void
  onUnauthorized: () => void
}

export function useSnapshotLoader(options: SnapshotLoaderOptions) {
  const loading = ref(false)
  const capabilityUnavailable = ref<string | null>(null)
  const canonicalUnavailable = ref<CanonicalUnavailableState | null>(null)
  const loadError = ref<string | null>(null)
  const refreshError = ref<string | null>(null)
  let loadController: AbortController | null = null
  let eventStream: EventStreamClient | null = null

  async function loadSnapshot(preserveDirty: boolean): Promise<void> {
    const token = options.accessToken.value
    if (!token) return
    loadController?.abort()
    const controller = new AbortController()
    loadController = controller
    loading.value = true
    capabilityUnavailable.value = null
    loadError.value = null
    try {
      const next = await fetchConfig(token, controller.signal)
      if (controller.signal.aborted) return
      canonicalUnavailable.value = null
      refreshError.value = null
      if (options.syncSnapshot(next, !preserveDirty)) options.ensureSelection()
      ensureEventStream()
    } catch (error) {
      if (controller.signal.aborted) return
      if (error instanceof ApiError && error.status === 401) {
        options.onUnauthorized()
        return
      }
      const unavailable = canonicalUnavailablePayload(error)
      if (unavailable) {
        canonicalUnavailable.value = unavailable
        options.diskDiagnostics.value = unavailable.diagnostics
        if (options.snapshot.value) options.staleRevision.value = unavailable.diskRevision ?? 'unknown'
        return
      }
      const message = error instanceof Error ? error.message : 'The configuration endpoint did not respond.'
      if (options.snapshot.value) {
        refreshError.value = message
      } else {
        if (configRouteUnavailable(error)) capabilityUnavailable.value = message
        else loadError.value = message
        options.draft.value = null
      }
    } finally {
      if (loadController === controller) {
        loadController = null
        loading.value = false
      }
    }
  }

  function ensureEventStream(): void {
    if (eventStream || !options.accessToken.value || !options.snapshot.value) return
    const client = connectEventStream(options.accessToken.value, {
      onEvent: (event) => {
        if (event.revision !== null) void loadSnapshot(true)
      },
      onResyncRequired: async () => {
        await loadSnapshot(true)
      },
      onError: (error) => {
        if (error instanceof ApiError && error.status === 401) options.onUnauthorized()
      },
    })
    eventStream = client
    void client.closed.then(() => {
      if (eventStream === client) eventStream = null
    })
  }

  function stop(): void {
    eventStream?.close()
    eventStream = null
    loadController?.abort()
  }

  return {
    loading,
    capabilityUnavailable,
    canonicalUnavailable,
    loadError,
    refreshError,
    loadSnapshot,
    ensureEventStream,
    stop,
  }
}
