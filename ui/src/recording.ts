import type { RecorderSnapshot, StreamSnapshot, TrackSnapshot } from './api'

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
