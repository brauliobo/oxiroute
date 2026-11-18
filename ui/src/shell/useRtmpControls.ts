import { ref } from 'vue'
import type { Ref } from 'vue'

import { ApiError, dropRtmpClient as requestRtmpClientDrop, setRecording } from '../api'
import type {
  RecorderSnapshot,
  RtmpCatalog,
  RtmpClientControlTarget,
  RtmpClientSnapshot,
  StreamSnapshot,
} from '../api'
import { presentApiError } from '../formatters'
import { recorderControlAction } from '../recording'

interface RtmpControlOptions {
  token: Ref<string>
  catalog: Ref<RtmpCatalog | null>
  catalogError: Ref<string | null>
  rtmpStatsError: Ref<string | null>
  refreshAfterCommand: () => Promise<void>
  onUnauthorized: () => void
}

const isConflict = (error: unknown) => error instanceof ApiError && [404, 409].includes(error.status)
const isUnauthorized = (error: unknown) => error instanceof ApiError && error.status === 401

export function useRtmpControls(options: RtmpControlOptions) {
  const { token, catalog, catalogError, rtmpStatsError, refreshAfterCommand, onUnauthorized } = options
  const busyRecorder = ref<string | null>(null)
  const busySessionId = ref<string | null>(null)

  const streamPresent = (id: string) => catalog.value?.streams.some((stream) => stream.id === id) ?? false

  async function controlRecorder(stream: StreamSnapshot, recorder: RecorderSnapshot): Promise<void> {
    const action = recorderControlAction(catalog.value?.capabilities.manual_recording ?? false, stream, recorder)
    if (!action) {
      catalogError.value = 'Recorder state changed before the command could be sent. Refreshing the active stream state.'
      await refreshAfterCommand()
      return
    }
    busyRecorder.value = recorder.id
    catalogError.value = null
    try {
      const result = await setRecording(stream.id, recorder.id, action, token.value)
      if (result.id !== recorder.id) throw new Error('Recorder command returned a mismatched recorder identity.')
      await refreshAfterCommand()
      if (!streamPresent(stream.id)) {
        catalogError.value = staleRecorderMessage(stream)
      } else if (!catalog.value?.streams.find(({ id }) => id === stream.id)?.recorders.some(({ id }) => id === recorder.id)) {
        catalogError.value = 'The recorder no longer exists on the active stream. No command success was assumed.'
      }
    } catch (requestError) {
      if (isUnauthorized(requestError)) return onUnauthorized()
      if (isConflict(requestError)) await refreshAfterCommand()
      catalogError.value = recorderControlError(requestError)
    } finally {
      if (busyRecorder.value === recorder.id) busyRecorder.value = null
    }
  }

  async function dropRtmpClient(client: RtmpClientSnapshot, target: RtmpClientControlTarget): Promise<void> {
    busySessionId.value = client.id
    rtmpStatsError.value = null
    try {
      const result = await requestRtmpClientDrop(client, target, token.value)
      if (result.sessionId !== client.id || result.target !== target) {
        throw new Error('RTMP control returned a mismatched session identity.')
      }
      await refreshAfterCommand()
    } catch (requestError) {
      if (isUnauthorized(requestError)) return onUnauthorized()
      if (isConflict(requestError)) await refreshAfterCommand()
      rtmpStatsError.value = rtmpControlError(requestError)
    } finally {
      if (busySessionId.value === client.id) busySessionId.value = null
    }
  }

  function staleRecorderMessage(stream: StreamSnapshot): string {
    const replacement = catalog.value?.streams.some((candidate) =>
      candidate.server_id === stream.server_id &&
      candidate.application === stream.application &&
      candidate.name === stream.name,
    )
    return replacement
      ? 'The publisher stream was replaced while the recorder command was in flight. The replacement state is shown; no command success was assumed.'
      : 'The publisher stream ended while the recorder command was in flight. No command success was assumed.'
  }

  return { busyRecorder, busySessionId, controlRecorder, dropRtmpClient }
}

function rtmpControlError(error: unknown): string {
  if (!(error instanceof ApiError)) return presentApiError(error, 'RTMP client control failed')
  switch (error.code) {
    case 'session_not_found':
      return 'The RTMP client ended before the control request was applied.'
    case 'session_revision_conflict':
    case 'session_role_conflict':
      return 'The RTMP client state changed before the control request was applied. Refreshing the current state.'
    case 'session_control_pending':
      return 'A control request is already pending for this RTMP client.'
    default:
      return `RTMP client control failed. ${error.message}`
  }
}

function recorderControlError(error: unknown): string {
  if (!(error instanceof ApiError)) return presentApiError(error, 'Recorder command failed')
  switch (error.code) {
    case 'rtmp_recording_unavailable':
      return 'Manual recording is unavailable in the active runtime.'
    case 'rtmp_recorder_start_failed':
      return 'Recorder command failed. The recorder could not be started.'
    case 'rtmp_recorder_stop_failed':
      return 'Recorder command failed. The recorder could not be stopped.'
    case 'rtmp_resource_not_found':
      return 'The target stream or recorder no longer exists. Active stream state was refreshed.'
    case 'rtmp_state_conflict':
      return `The recorder state changed before the command completed. ${error.message}`
    default:
      return `Recorder command failed. ${error.message}`
  }
}
