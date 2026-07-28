import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'

import type { RtmpRecorderConfig, RtmpServiceConfig } from '../config'
import RtmpRecorderEditor from './RtmpRecorderEditor.vue'
import RtmpServiceEditor from './RtmpServiceEditor.vue'

function recorder(): RtmpRecorderConfig {
  return {
    name: 'archive',
    start: 'continuous',
    root_directory: '/var/lib/oxiroute/recordings',
    suffix_template: '.flv',
    append_unix_seconds: false,
    timezone: 'utc',
    time_basis: 'segment_start',
    segment_naming: 'safe_unique',
    rotation_interval_ms: null,
    max_queue_messages: 256,
    max_queue_bytes: 8_388_608,
    shutdown_timeout_ms: 5_000,
    max_storage_bytes: 10_737_418_240,
    max_storage_files: 10_000,
    max_active_recorders: 8,
  }
}

describe('RTMP recorder configuration editors', () => {
  it('edits every recorder field with stable paths and bounded controls', async () => {
    const model = recorder()
    const wrapper = mount(RtmpRecorderEditor, { props: { recorder: model, index: 0 } })
    const field = (path: string) => wrapper.get(`[data-field="${path}"]`)

    await field('rtmp_services[].applications[].recorders[].name').get('input').setValue('manual-archive')
    await field('rtmp_services[].applications[].recorders[].start').get('select').setValue('manual')
    await field('rtmp_services[].applications[].recorders[].root_directory').get('input').setValue('/srv/recordings')
    await field('rtmp_services[].applications[].recorders[].suffix_template').get('input').setValue('-%Y%m%d.flv')
    await field('rtmp_services[].applications[].recorders[].append_unix_seconds').get('input').setValue(true)
    await field('rtmp_services[].applications[].recorders[].timezone').get('input').setValue('America/Recife')
    const rotation = field('rtmp_services[].applications[].recorders[].rotation_interval_ms')
    await rotation.get('select').setValue('bounded')
    await rotation.get('input').setValue(60_000)
    await field('rtmp_services[].applications[].recorders[].max_queue_messages').get('input').setValue(512)
    await field('rtmp_services[].applications[].recorders[].max_queue_bytes').get('input').setValue(16_777_216)
    await field('rtmp_services[].applications[].recorders[].shutdown_timeout_ms').get('input').setValue(6_000)
    await field('rtmp_services[].applications[].recorders[].max_storage_bytes').get('input').setValue(21_474_836_480)
    await field('rtmp_services[].applications[].recorders[].max_storage_files').get('input').setValue(20_000)
    await field('rtmp_services[].applications[].recorders[].max_active_recorders').get('input').setValue(16)

    expect(model).toEqual({
      name: 'manual-archive',
      start: 'manual',
      root_directory: '/srv/recordings',
      suffix_template: '-%Y%m%d.flv',
      append_unix_seconds: true,
      timezone: 'America/Recife',
      time_basis: 'segment_start',
      segment_naming: 'safe_unique',
      rotation_interval_ms: 60_000,
      max_queue_messages: 512,
      max_queue_bytes: 16_777_216,
      shutdown_timeout_ms: 6_000,
      max_storage_bytes: 21_474_836_480,
      max_storage_files: 20_000,
      max_active_recorders: 16,
    })
    expect(field('rtmp_services[].applications[].recorders[].max_queue_messages').get('input').attributes('max')).toBe('65536')
    expect(rotation.get('input').attributes('max')).toBe('2147483647')
  })

  it('adds default recorders, removes exact rows, and enforces the application limit', async () => {
    const service: RtmpServiceConfig = {
      name: 'live',
      outbound_chunk_size: 4_096,
      access_log: null,
      applications: [{ name: 'broadcast', live: true, idle_streams: true, push_targets: [], fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 }, recorders: [] }],
    }
    const wrapper = mount(RtmpServiceEditor, { props: { service } })
    const add = wrapper.get('.recorder-list .add-row')

    await add.trigger('click')
    expect(service.applications[0]?.recorders).toEqual([{ ...recorder(), name: '' }])
    await wrapper.get('[aria-label="Remove recorder 1"]').trigger('click')
    expect(service.applications[0]?.recorders).toEqual([])

    for (let index = 0; index < 8; index += 1) await add.trigger('click')
    expect(service.applications[0]?.recorders).toHaveLength(8)
    expect(add.attributes()).toHaveProperty('disabled')

    const playback = mount(RtmpServiceEditor, {
      props: {
        service: {
          name: 'playback',
          outbound_chunk_size: 4_096,
          access_log: null,
          applications: [{ name: 'playback', live: false, idle_streams: true, push_targets: [], fanout: { max_subscribers: 1_024, max_queue_messages_per_subscriber: 256, max_queue_bytes_per_subscriber: 8_388_608 }, recorders: [] }],
        },
      },
    })
    const disabledForPlayback = playback.get('.recorder-list .add-row')
    expect(disabledForPlayback.attributes()).toHaveProperty('disabled')
    expect(disabledForPlayback.attributes('title')).toContain('requires live publishing')
  })
})
