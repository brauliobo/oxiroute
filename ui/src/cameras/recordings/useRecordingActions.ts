import { computed } from 'vue'
import type { Recording } from '../../api'
import { useActionRunner } from '../../workspace/actions'
import { deletePlan, downloadPlan, startDownload, type PlanEnvironment, type RecordingScope } from './plans'

interface Options {
  token: () => string
  reload: () => Promise<void>
  onUnauthorized: () => void
}

export function useRecordingActions({ token, reload, onUnauthorized }: Options) {
  const common = { token, context: () => true as const, onUnauthorized }
  const downloads = useActionRunner<RecordingScope, true>({
    ...common,
    conflictHint: 'The recording changed since the list was loaded. Refresh the list and retry.',
    reload: () => Promise.resolve(),
  })
  const deletions = useActionRunner<RecordingScope, true>({
    ...common,
    conflictHint: 'The recording is still being written, so it cannot be deleted. Wait until its recorder stops. ' +
      'The list was reloaded.',
    reload,
  })

  const outcome = computed(() => downloads.outcome.value ?? deletions.outcome.value)
  const running = computed(() => downloads.running.value || deletions.running.value)
  const dismiss = (): void => {
    downloads.dismiss()
    deletions.dismiss()
  }
  const env: PlanEnvironment = { reload, startDownload, supersede: dismiss }

  return {
    outcome,
    running,
    dismiss,
    download: (recording: Recording) => downloads.perform(downloadPlan(recording, env)),
    remove: (recording: Recording) => deletions.perform(deletePlan(recording, env)),
  }
}
