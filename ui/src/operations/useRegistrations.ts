import { ref, shallowRef } from 'vue'
import { ApiError, fetchRegistrations, registryDisabled, type RegistrationList } from '../api'
import { presentApiError } from '../formatters'
import { useLatestAbortableTask } from '../useLatestAbortableTask'

export function useRegistrations(token: () => string, onUnauthorized: () => void) {
  const list = shallowRef<RegistrationList | null>(null)
  const error = ref<string | null>(null)
  const disabled = ref(false)
  const { loading, run, cancel } = useLatestAbortableTask()

  async function load(): Promise<void> {
    error.value = null
    await run(
      (signal) => fetchRegistrations(token(), signal),
      (next) => {
        list.value = next
        disabled.value = false
      },
      (failure) => {
        if (failure instanceof ApiError && failure.status === 401) onUnauthorized()
        disabled.value = registryDisabled(failure)
        if (disabled.value) list.value = null
        error.value = disabled.value ? null : presentApiError(failure, 'The registry API did not respond.')
      },
    )
  }

  function reset(): void {
    cancel()
    list.value = null
    error.value = null
    disabled.value = false
  }

  return { list, error, disabled, loading, load, reset }
}
