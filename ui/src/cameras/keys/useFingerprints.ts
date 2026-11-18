import { ref, shallowRef } from 'vue'
import { ApiError, fetchSecretFingerprints, type SecretFingerprint, type SecretRole } from '../../api'
import { presentApiError } from '../../formatters'
import { useLatestAbortableTask } from '../../useLatestAbortableTask'

export const ROLE_LABELS: Readonly<Record<SecretRole, string>> = { publish: 'Publish key', play: 'Play key' }

export function useFingerprints(token: () => string, onUnauthorized: () => void) {
  const fingerprints = shallowRef<readonly SecretFingerprint[] | null>(null)
  const error = ref<string | null>(null)
  const { loading, run, cancel } = useLatestAbortableTask()

  async function load(): Promise<void> {
    error.value = null
    await run(
      (signal) => fetchSecretFingerprints(token(), signal),
      (next) => { fingerprints.value = next },
      (failure) => {
        if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
        error.value = presentApiError(failure, 'The keys API did not respond.')
      },
    )
  }

  function reset(): void {
    cancel()
    fingerprints.value = null
    error.value = null
  }

  return { fingerprints, error, loading, load, reset }
}
