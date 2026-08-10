import { onBeforeUnmount, readonly, ref } from 'vue'

export function useLatestAbortableTask() {
  const loading = ref(false)
  let controller: AbortController | null = null
  let generation = 0

  async function run<T>(
    task: (signal: AbortSignal) => Promise<T>,
    onSuccess: (result: T) => void,
    onError: (error: unknown) => void,
  ): Promise<boolean> {
    controller?.abort()
    const nextController = new AbortController()
    const nextGeneration = ++generation
    controller = nextController
    loading.value = true

    try {
      const result = await task(nextController.signal)
      if (!isCurrent(nextGeneration, nextController)) return false
      onSuccess(result)
    } catch (error) {
      if (!isCurrent(nextGeneration, nextController)) return false
      onError(error)
    } finally {
      if (isCurrent(nextGeneration, nextController)) {
        controller = null
        loading.value = false
      }
    }

    return isCurrent(nextGeneration, nextController)
  }

  function cancel(): void {
    generation += 1
    controller?.abort()
    controller = null
    loading.value = false
  }

  function isCurrent(taskGeneration: number, taskController: AbortController): boolean {
    return generation === taskGeneration && !taskController.signal.aborted
  }

  onBeforeUnmount(cancel)

  return { loading: readonly(loading), run, cancel }
}
