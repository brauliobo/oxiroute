import type { Tone } from '#design/badge/tone'
import type { RecorderSnapshot, RtmpRelayPhase, StreamSnapshot, TrackSnapshot } from './api'

export function hasObservedCodec(track: TrackSnapshot): boolean {
  return track.codec_id !== null || track.codec_fourcc !== null
}

export function streamRecordingSupported(stream: StreamSnapshot): boolean {
  if (!stream.recording_supported) return false
  const observedTracks = [stream.media.audio, stream.media.video].filter(hasObservedCodec)
  return observedTracks.length > 0 && observedTracks.every((track) => track.recording_supported)
}

export function recorderControlAction(
  manualRecording: boolean,
  stream: StreamSnapshot,
  recorder: RecorderSnapshot,
): 'start' | 'stop' | null {
  if (!manualRecording || !stream.manual_recording || !recorder.manual || !stream.publisher) return null
  if (recorder.phase.state === 'recording') return 'stop'
  return streamRecordingSupported(stream) && ['idle', 'failed'].includes(recorder.phase.state)
    ? 'start'
    : null
}

const RECORDER_PHASE_TONES: Record<RecorderSnapshot['phase']['state'], Tone> = {
  idle: 'neutral',
  starting: 'warn',
  recording: 'ok',
  stopping: 'warn',
  failed: 'bad',
}

const RELAY_PHASE_TONES: Record<RtmpRelayPhase, Tone> = {
  connecting: 'warn',
  publishing: 'ok',
  pulling: 'ok',
  backoff: 'warn',
  stopped: 'neutral',
}

export const recorderPhaseTone = (state: RecorderSnapshot['phase']['state']): Tone => RECORDER_PHASE_TONES[state]
export const relayPhaseTone = (phase: RtmpRelayPhase): Tone => RELAY_PHASE_TONES[phase]
