import type { Recording } from '../../api'
import { useActionRunner } from '../../workspace/actions'
import { deletePlan, downloadPlan, startDownload, type PlanEnvironment, type RecordingScope } from './plans'

interface Options {
  token: () => string
  reload: () => Promise<void>
  onUnauthorized: () => void
}

export function useRecordingActions({ token, reload, onUnauthorized }: Options) {
  const runner = useActionRunner<RecordingScope, true>({
    token,
    context: () => true,
    conflictHint: 'The recording changed since the list was loaded, for example it is still being written and ' +
      'cannot be deleted until its recorder stops. The list was reloaded.',
    reload,
    onUnauthorized,
  })
  const env: PlanEnvironment = { reload, startDownload, supersede: runner.dismiss }

  return {
    outcome: runner.outcome,
    running: runner.running,
    dismiss: runner.dismiss,
    download: (recording: Recording) => runner.perform(downloadPlan(recording, env)),
    remove: (recording: Recording) => runner.perform(deletePlan(recording, env)),
  }
}
