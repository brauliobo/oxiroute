import { UNKNOWN } from '#design/format'
import { ApiError, createRecordingTicket, deleteRecording, type Recording } from '../../api'
import type { ConfirmRequest } from '../../shell/confirm'
import type { ActionPlan } from '../../workspace/actions'
import { sizeText, startedText } from './text'

export const RECORDINGS_SCOPE = 'recordings'

export const GONE = 'The recording no longer exists; the list was reloaded'

export const ACTIVE_REASON = 'It is being written now and cannot be deleted until its recorder stops.'

export type RecordingScope = typeof RECORDINGS_SCOPE

export type RecordingPlan<Result> = ActionPlan<RecordingScope, true, Result>

export interface PlanEnvironment {
  reload: () => Promise<void>
  startDownload: (path: string, name: string) => void
  supersede: () => void
}

export const startDownload = (path: string, name: string): void => {
  const link = document.createElement('a')
  link.href = path
  link.download = name
  link.hidden = true
  document.body.append(link)
  link.click()
  link.remove()
}

async function reloadWhenGone<T>(action: () => Promise<T>, reload: () => Promise<void>): Promise<T> {
  try {
    return await action()
  } catch (error) {
    if (!(error instanceof ApiError && error.code === 'recording_not_found')) throw error
    await reload()
    throw new Error(GONE)
  }
}

export const deleteConfirmation = (recording: Recording): ConfirmRequest => ({
  title: `Delete ${recording.name}`,
  message: 'This deletes the recording file and its metadata sidecar. It cannot be undone.',
  confirmLabel: 'Delete recording',
  danger: true,
  details: [`Stream: ${recording.stream ?? UNKNOWN}`, `Size: ${sizeText(recording)}`, `Started: ${startedText(recording)}`],
})

export const downloadPlan = (recording: Recording, env: PlanEnvironment): RecordingPlan<void> => ({
  scope: RECORDINGS_SCOPE,
  failure: `download ${recording.name}`,
  reloadOnSuccess: false,
  run: (_, token) => reloadWhenGone(async () => {
    env.supersede()
    const { path } = await createRecordingTicket(recording.id, token)
    env.startDownload(path, recording.name)
  }, env.reload),
  succeeded: () => ({
    tone: 'ok',
    summary: `Download of ${recording.name} started.`,
    details: ['The browser saves the file. Open it in a media player: browsers cannot play FLV.'],
  }),
})

export const deletePlan = (recording: Recording, env: PlanEnvironment): RecordingPlan<Recording> => ({
  scope: RECORDINGS_SCOPE,
  failure: `delete ${recording.name}`,
  confirm: deleteConfirmation(recording),
  run: (_, token) => reloadWhenGone(() => {
    env.supersede()
    return deleteRecording(recording.id, token)
  }, env.reload),
  succeeded: () => ({
    tone: 'ok',
    summary: `Deleted ${recording.name} and its metadata sidecar.`,
  }),
})
