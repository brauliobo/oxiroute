import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import RtmpRecorderPanel from './RtmpRecorderPanel.vue'
import type { RecorderSnapshot, StreamSnapshot } from './api'

function recorder(overrides: Partial<RecorderSnapshot> = {}): RecorderSnapshot {
  return {
    id: 'recorder-1',
    name: 'archive',
    manual: true,
    phase: { state: 'idle' },
    changed_at_unix_ms: 1_750_000_000_000,
    bytes_written: '1048576',
    current_relative_name: 'live/.camera-current.partial',
    published_but_not_durable_relative_name: null,
    segments_started: '3',
    segments_completed: '2',
    discontinuities: '1',
    last_completed_relative_name: 'live/camera-001.flv',
    recoverable_partial_name: 'live/.camera-002.partial',
    ...overrides,
  }
}

function stream(recorders: RecorderSnapshot[] = [recorder()]): StreamSnapshot {
  return {
    id: 'stream-1',
    revision: '1',
    server_id: 'edge',
    application: 'live',
    name: 'camera',
    created_at_unix_ms: 1_750_000_000_000,
    publisher: { session_id: 'publisher-1', attached_at_unix_ms: 1_750_000_000_000 },
    subscriber_count: 0,
    media: {
      audio: {
        codec_id: 10,
        codec_fourcc: null,
        codec_name: 'aac',
        recording_supported: true,
        payload_bytes: '1024',
        last_rtmp_timestamp_ms: 1,
        last_observed_at_unix_ms: 1_750_000_000_000,
      },
      video: {
        codec_id: 7,
        codec_fourcc: null,
        codec_name: 'avc',
        recording_supported: true,
        payload_bytes: '4096',
        last_rtmp_timestamp_ms: 1,
        last_observed_at_unix_ms: 1_750_000_000_000,
      },
      fanout_payload_bytes: '0',
    },
    recording_supported: true,
    manual_recording: true,
    recorders,
  }
}

describe('RtmpRecorderPanel', () => {
  it('shows recorder status and emits controls only for supported manual idle phases', async () => {
    const continuous = recorder({ id: 'recorder-2', name: 'continuous', manual: false })
    const wrapper = mount(RtmpRecorderPanel, {
      props: {
        stream: stream([recorder(), continuous]),
        manualRecording: true,
        busyRecorderId: null,
      },
    })

    expect(wrapper.text()).toContain('Recording supported')
    expect(wrapper.text()).toContain('Manual')
    expect(wrapper.text()).toContain('Continuous')
    expect(wrapper.text()).toContain('2 segments')
    expect(wrapper.text()).toContain('1 discontinuity')
    expect(wrapper.text()).toContain('live/camera-001.flv')
    expect(wrapper.findAll('[data-recorder-action]')).toHaveLength(1)

    const action = wrapper.get('[data-recorder-action]')
    expect(action.text()).toBe('Start recording')
    expect(action.attributes('disabled')).toBeUndefined()
    await action.trigger('click')
    expect(wrapper.emitted('control')?.[0]?.[0]).toEqual(expect.objectContaining({ id: 'recorder-1' }))
  })

  it('disables unsupported starts and transitional phases but permits an exact recording stop', async () => {
    const unsupported = stream([recorder()])
    unsupported.media.video = {
      ...unsupported.media.video,
      codec_id: null,
      codec_fourcc: 'hvc1',
      codec_name: 'hevc',
      recording_supported: false,
    }
    const wrapper = mount(RtmpRecorderPanel, {
      props: { stream: unsupported, manualRecording: true, busyRecorderId: null },
    })

    expect(wrapper.get('[data-recorder-action]').attributes()).toHaveProperty('disabled')
    expect(wrapper.text()).toContain('Active codec cannot be recorded')

    const recording = stream([recorder({
      phase: { state: 'recording', operation_id: 'operation-1', started_at_unix_ms: 1 },
    })])
    recording.media.video = unsupported.media.video
    await wrapper.setProps({ stream: recording })
    expect(wrapper.get('[data-recorder-action]').text()).toBe('Stop recording')
    expect(wrapper.get('[data-recorder-action]').attributes('disabled')).toBeUndefined()

    await wrapper.setProps({
      stream: stream([recorder({ phase: { state: 'starting', operation_id: 'operation-2' } })]),
    })
    expect(wrapper.get('[data-recorder-action]').attributes()).toHaveProperty('disabled')
  })

  it('reflects a runtime without manual recording capability', () => {
    const wrapper = mount(RtmpRecorderPanel, {
      props: { stream: stream(), manualRecording: false, busyRecorderId: null },
    })

    expect(wrapper.get('[data-recorder-action]').text()).toBe('Manual control unavailable')
    expect(wrapper.get('[data-recorder-action]').attributes()).toHaveProperty('disabled')
  })

  it('does not describe an unconfigured recorder as a codec failure', () => {
    const withoutRecorder = stream([])
    withoutRecorder.recording_supported = false
    withoutRecorder.manual_recording = false
    const wrapper = mount(RtmpRecorderPanel, {
      props: { stream: withoutRecorder, manualRecording: false, busyRecorderId: null },
    })

    expect(wrapper.text()).toContain('No recorder configured')
    expect(wrapper.text()).not.toContain('cannot be recorded')
    expect(wrapper.find('[data-recorder-action]').exists()).toBe(false)
  })
})
