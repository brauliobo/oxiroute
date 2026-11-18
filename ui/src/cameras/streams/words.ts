import type { RecorderErrorCode, RecorderSnapshot, RtmpEndReason } from '../../api'

export const RECORDER_FAILURES: Readonly<Record<RecorderErrorCode, string>> = {
  open_failed: 'the recording file could not be opened',
  quota_files: 'the folder reached its file limit and its retention does not free space (quota_files)',
  quota_bytes: 'the folder reached its size limit and its retention does not free space (quota_bytes)',
  write_failed: 'writing to disk failed',
  close_failed: 'the recording file could not be closed',
  backend_unavailable: 'the recording backend is not available',
  file_sync_failed: 'the recording file could not be flushed to disk',
  publish_failed: 'a finished recording could not be published',
  directory_sync_failed: 'the recording folder could not be flushed to disk',
  queue_discontinuity: 'the recorder fell behind and dropped media',
  unsupported_codec: 'the stream uses a codec the recorder cannot store',
  shutdown_timed_out: 'the recorder did not stop in time',
  worker_panicked: 'the recorder stopped unexpectedly',
  stale_publisher: 'the publisher stopped sending media',
}

export const END_REASONS: Readonly<Record<RtmpEndReason, string>> = {
  closed: 'The publisher disconnected.',
  dropped: 'An operator dropped the publisher.',
  stale: 'The publisher stopped sending media.',
  shutdown: 'The server was shutting down.',
  failed: 'A transport or protocol error ended the session.',
}

export const RECORDER_PHASES: Readonly<Record<RecorderSnapshot['phase']['state'], string>> = {
  idle: 'IDLE',
  starting: 'STARTING',
  recording: 'RECORDING',
  stopping: 'STOPPING',
  failed: 'FAILED',
}
