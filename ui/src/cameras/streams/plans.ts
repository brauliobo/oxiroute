import { formatDuration } from '#design/format'
import { dropRtmpClient, setRecording } from '../../api'
import type { RtmpControlResponse, RtmpClientSnapshot } from '../../api'
import { pluralize } from '../../formatters'
import type { ActionPlan } from '../../workspace/actions'
import type { Camera, RecorderView } from './model'

export type StreamsScope = 'streams'
export type StreamPlan<Result> = ActionPlan<StreamsScope, true, Result>

const SCOPE = 'streams'

export function startRecording(camera: Camera, recorder: RecorderView): StreamPlan<unknown> {
  return {
    scope: SCOPE,
    failure: `start recording ${camera.title}`,
    run: (_, token) => setRecording(camera.id, recorder.id, 'start', token),
    succeeded: () => ({ tone: 'ok', summary: `Recording ${camera.title} with ${recorder.name} was requested.` }),
  }
}

export function stopRecording(camera: Camera, recorder: RecorderView): StreamPlan<unknown> {
  return {
    scope: SCOPE,
    failure: `stop recording ${camera.title}`,
    confirm: {
      title: `Stop recording ${camera.title}`,
      message: `The recorder ${recorder.name} finishes the file it is writing and stops. The stream itself keeps ` +
        'running. Start the recorder again to continue in a new file.',
      confirmLabel: 'Stop recording',
      danger: true,
      details: [`Stream: ${camera.title}`, ...(recorder.file ? [`Writing: ${recorder.file}`] : [])],
    },
    run: (_, token) => setRecording(camera.id, recorder.id, 'stop', token),
    succeeded: () => ({ tone: 'ok', summary: `Stopping the recording of ${camera.title} (${recorder.name}) was requested.` }),
  }
}

const dropped = (subject: string) => (response: RtmpControlResponse) =>
  response.outcome === 'already_requested'
    ? { tone: 'warn' as const, summary: `Dropping ${subject} was already requested.` }
    : { tone: 'ok' as const, summary: `${subject} was asked to disconnect.` }

export function dropPublisher(camera: Camera, client: RtmpClientSnapshot): StreamPlan<RtmpControlResponse> {
  const subject = `The publisher of ${camera.title}`
  return {
    scope: SCOPE,
    failure: `drop the publisher of ${camera.title}`,
    confirm: {
      title: `Drop the publisher of ${camera.title}`,
      message: 'The publisher is disconnected now. The camera or encoder has to reconnect before the stream is ' +
        'live again, and recorders on this stream stop with it.',
      confirmLabel: 'Drop publisher',
      danger: true,
      details: [
        `Publisher: ${client.peerIp ?? 'unknown address'}`,
        `Connected for ${formatDuration(camera.uptimeMs ?? 0)}`,
        `${pluralize(camera.viewers, 'viewer')} will lose the stream`,
      ],
    },
    run: (_, token) => dropRtmpClient(client, 'publisher', token),
    succeeded: dropped(subject),
  }
}

export function dropViewer(camera: Camera, client: RtmpClientSnapshot): StreamPlan<RtmpControlResponse> {
  const who = client.peerIp ?? 'a viewer'
  return {
    scope: SCOPE,
    failure: `drop the viewer ${who} of ${camera.title}`,
    confirm: {
      title: `Drop the viewer ${who}`,
      message: `The viewer is disconnected from ${camera.title}. A player may reconnect on its own.`,
      confirmLabel: 'Drop viewer',
      danger: true,
      details: [`Stream: ${camera.title}`, `Session: ${client.id}`],
    },
    run: (_, token) => dropRtmpClient(client, 'subscriber', token),
    succeeded: dropped(`The viewer ${who} of ${camera.title}`),
  }
}
